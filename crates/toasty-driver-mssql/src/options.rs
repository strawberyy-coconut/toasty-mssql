//! Connection configuration for SQL Server, parsed from an `mssql://` URL.
//!
//! [`MssqlConnectOptions`] is the driver's own configuration type: it takes the
//! URL form [`Mssql`](crate::Mssql) is built from, and turns it into the
//! `mssql-tds` [`ClientContext`] the TDS client connects with. The URL is the
//! same shape the reference `sqlx` MSSQL driver accepted, so an existing
//! connection string keeps working.

use std::{fmt, path::PathBuf, str::FromStr, time::Duration};

use mssql_tds::{
    connection::client_context::ClientContext,
    core::{EncryptionOptions, EncryptionSetting},
};
use toasty_core::{Error, Result, driver::ConnectionUrl};

/// The port SQL Server listens on by default.
const DEFAULT_PORT: u16 = 1433;

/// Connection options for a SQL Server database.
///
/// The usual way to build them is [`parse`](Self::parse), from a `mssql://`
/// URL:
///
/// ```text
/// mssql://sa:Password1!@localhost:1433/mydb?encrypt=on&trust_certificate=true
/// ```
///
/// Recognised query parameters:
///
/// | Parameter | Meaning |
/// |-----------|---------|
/// | `database` | Database name, overriding the URL path |
/// | `encrypt` | `on`, `required`, `strict` or `off` (also `true`/`false`) |
/// | `trust_certificate` | `true` skips server certificate validation |
/// | `server_certificate` | Path to a DER or PEM certificate to pin |
/// | `host_name_in_cert` | CN or SAN expected in the server certificate |
/// | `application_name` | Application name reported to the server |
/// | `connect_timeout` | Connection timeout in seconds |
///
/// Unknown parameters are ignored, as the reference driver did, so a URL
/// written for another client still parses.
///
/// The default encryption mode is [`EncryptionSetting::Strict`] — the same
/// default [`ClientContext`] has. Strict is TDS 8.0, which SQL Server 2022 does
/// not speak, so a URL pointing at one wants `?encrypt=on`.
#[derive(Clone)]
pub struct MssqlConnectOptions {
    host: String,
    port: u16,
    username: String,
    password: String,
    database: String,
    encryption: EncryptionSetting,
    trust_server_certificate: bool,
    server_certificate: Option<PathBuf>,
    host_name_in_cert: Option<String>,
    application_name: Option<String>,
    connect_timeout: Option<Duration>,
}

impl fmt::Debug for MssqlConnectOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MssqlConnectOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            // Never render the password, even accidentally.
            .field("password", &"<redacted>")
            .field("database", &self.database)
            .field("encryption", &self.encryption)
            .field("trust_server_certificate", &self.trust_server_certificate)
            .field("server_certificate", &self.server_certificate)
            .field("host_name_in_cert", &self.host_name_in_cert)
            .field("application_name", &self.application_name)
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

impl Default for MssqlConnectOptions {
    /// The values a `mssql://` URL starts from, which are the ones
    /// [`ClientContext`] itself defaults to.
    fn default() -> Self {
        Self {
            host: "localhost".to_owned(),
            port: DEFAULT_PORT,
            username: String::new(),
            password: String::new(),
            database: String::new(),
            // `ClientContext`'s own default, rather than the reference driver's
            // `On`: a URL that says nothing about encryption asks for what the
            // client asks for by default.
            encryption: EncryptionSetting::Strict,
            trust_server_certificate: false,
            server_certificate: None,
            host_name_in_cert: None,
            application_name: None,
            connect_timeout: None,
        }
    }
}

impl MssqlConnectOptions {
    /// Parses an `mssql://` URL.
    ///
    /// The password is percent-decoded, so a credential containing a reserved
    /// character is written as its escape (`p%40ss` for `p@ss`).
    ///
    /// ```text
    /// let options = MssqlConnectOptions::parse(
    ///     "mssql://sa:Password1!@localhost:1433/mydb?encrypt=on",
    /// )?;
    /// assert_eq!(options.database(), "mydb");
    /// ```
    pub fn parse(url: &str) -> Result<Self> {
        let parsed = ConnectionUrl::parse(url)?;

        if !parsed.has_scheme("mssql") {
            // Only the scheme is named: a string that reached this point without
            // an `mssql://` authority is not redactable, so echoing it could
            // print a credential.
            return Err(Error::invalid_connection_url(format!(
                "connection URL does not have an `mssql` scheme; found `{}`",
                parsed.scheme()
            )));
        }

        let host = parsed
            .host()?
            .filter(|host| !host.is_empty())
            .ok_or_else(|| {
                Error::invalid_connection_url(format!(
                    "MSSQL connection URL is missing a host; url={}",
                    parsed.redact_password()
                ))
            })?
            .to_owned();

        let mut options = Self {
            host,
            port: parsed.port()?.unwrap_or(DEFAULT_PORT),
            username: parsed.username()?.unwrap_or_default().into_owned(),
            password: parsed
                .password()
                .map(|password| String::from_utf8_lossy(&password).into_owned())
                .unwrap_or_default(),
            database: parsed.decoded_path()?.trim_start_matches('/').to_owned(),
            ..Self::default()
        };

        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "database" => options.database = value.into_owned(),
                "encrypt" => {
                    options.encryption = parse_encryption(&parsed.redact_password(), &value)?;
                }
                "trust_certificate" => {
                    options.trust_server_certificate =
                        parse_bool(&parsed.redact_password(), &key, &value)?;
                }
                "server_certificate" => {
                    options.server_certificate = Some(PathBuf::from(value.into_owned()));
                }
                "host_name_in_cert" => options.host_name_in_cert = Some(value.into_owned()),
                "application_name" => options.application_name = Some(value.into_owned()),
                "connect_timeout" => {
                    let seconds: u64 = value.parse().map_err(|_| {
                        Error::invalid_connection_url(format!(
                            "`connect_timeout` must be a whole number of seconds, got `{value}`; \
                             url={}",
                            parsed.redact_password()
                        ))
                    })?;
                    options.connect_timeout = Some(Duration::from_secs(seconds));
                }
                // Unknown parameters are ignored so that a URL can carry
                // settings meant for other drivers.
                _ => {}
            }
        }

        Ok(options)
    }

    /// The data source string `mssql-tds` connects with: `tcp:host,port`.
    ///
    /// An IPv6 literal keeps its brackets, so the comma that separates the port
    /// from the host is unambiguous.
    pub fn data_source(&self) -> String {
        if self.host.contains(':') {
            format!("tcp:[{}],{}", self.host, self.port)
        } else {
            format!("tcp:{},{}", self.host, self.port)
        }
    }

    /// The `mssql-tds` configuration these options describe.
    ///
    /// The context is assembled field by field rather than with struct-update
    /// syntax, because `ClientContext` holds private fields a caller cannot
    /// name.
    pub fn to_client_context(&self) -> ClientContext {
        let mut context = ClientContext::with_data_source(&self.data_source());

        context.user_name = self.username.clone();
        context.password = self.password.clone();
        context.database = self.database.clone();
        context.encryption_options = EncryptionOptions {
            mode: self.encryption,
            trust_server_certificate: self.trust_server_certificate,
            host_name_in_cert: self.host_name_in_cert.clone(),
            server_certificate: self.server_certificate.clone(),
        };

        if let Some(application_name) = &self.application_name {
            context.application_name = application_name.clone();
        }

        if let Some(timeout) = self.connect_timeout {
            context.connect_timeout = u32::try_from(timeout.as_secs()).unwrap_or(u32::MAX);
        }

        context
    }

    /// The server host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The server port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The database these options connect to.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// A copy of these options pointing at a different database.
    ///
    /// The driver connects to `master` for `reset_db`, and a test names the
    /// database it wants rather than the one in the URL, so both need the same
    /// credentials pointed somewhere else.
    pub fn with_database(&self, database: &str) -> Self {
        let mut options = self.clone();
        options.database = database.to_owned();
        options
    }

    /// Sets how the connection encrypts its traffic.
    pub fn encryption(&mut self, mode: EncryptionSetting) -> &mut Self {
        self.encryption = mode;
        self
    }

    /// Requests an encrypted connection.
    ///
    /// Shorthand for [`encryption`](Self::encryption), using
    /// [`EncryptionSetting::On`] when `enabled` and
    /// [`EncryptionSetting::PreferOff`] otherwise.
    pub fn encrypt(&mut self, enabled: bool) -> &mut Self {
        self.encryption = if enabled {
            EncryptionSetting::On
        } else {
            EncryptionSetting::PreferOff
        };
        self
    }

    /// Skips server certificate validation.
    ///
    /// This makes the connection vulnerable to man-in-the-middle attacks and
    /// should only be used against a server you trust on a private network.
    pub fn trust_certificate(&mut self, enabled: bool) -> &mut Self {
        self.trust_server_certificate = enabled;
        self
    }

    /// Pins the server's certificate instead of validating its chain.
    ///
    /// The file must hold a DER or PEM encoded X.509 certificate, which is
    /// matched byte for byte against the certificate the server presents.
    pub fn server_certificate(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.server_certificate = Some(path.into());
        self
    }

    /// Sets the CN or SAN the server certificate must carry.
    pub fn host_name_in_cert(&mut self, name: impl Into<String>) -> &mut Self {
        self.host_name_in_cert = Some(name.into());
        self
    }

    /// Sets the application name reported to the server.
    pub fn application_name(&mut self, name: impl Into<String>) -> &mut Self {
        self.application_name = Some(name.into());
        self
    }

    /// Sets the connection timeout.
    pub fn connect_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.connect_timeout = Some(timeout);
        self
    }
}

impl FromStr for MssqlConnectOptions {
    type Err = Error;

    fn from_str(url: &str) -> Result<Self> {
        Self::parse(url)
    }
}

/// Parses the `encrypt` query parameter.
///
/// The boolean spellings are still accepted, so URLs written before the
/// stricter modes existed keep working.
fn parse_encryption(redacted_url: &str, value: &str) -> Result<EncryptionSetting> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => return Ok(EncryptionSetting::On),
        "required" => return Ok(EncryptionSetting::Required),
        "strict" => return Ok(EncryptionSetting::Strict),
        "off" | "prefer-off" | "prefer_off" | "false" | "no" | "0" => {
            return Ok(EncryptionSetting::PreferOff);
        }
        _ => {}
    }

    Err(Error::invalid_connection_url(format!(
        "`encrypt` must be one of `on`, `required`, `strict` or `off`, got `{value}`; \
         url={redacted_url}"
    )))
}

/// Parses a boolean query parameter.
fn parse_bool(redacted_url: &str, key: &str, value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        other => Err(Error::invalid_connection_url(format!(
            "`{key}` must be a boolean, got `{other}`; url={redacted_url}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The URL the development container's server answers to.
    const URL: &str = "mssql://sa:Password1!@db:1433/testdb?encrypt=on&trust_certificate=true";

    #[test]
    fn defaults_are_the_client_context_defaults() {
        let options = MssqlConnectOptions::default();

        assert_eq!(options.host(), "localhost");
        assert_eq!(options.port(), DEFAULT_PORT);
        assert_eq!(options.database(), "");
        assert_eq!(options.encryption, EncryptionSetting::Strict);
        assert!(!options.trust_server_certificate);
        assert_eq!(options.data_source(), "tcp:localhost,1433");
    }

    #[test]
    fn defaults_of_an_options_value_agree_with_the_client_context() {
        // The two constructions the driver offers — `Mssql::new` from a
        // `ClientContext` and `Mssql::from_options` from these options — must
        // describe the same connection when neither is configured.
        let from_options = MssqlConnectOptions::default().to_client_context();
        let direct = ClientContext::with_data_source("tcp:localhost,1433");

        assert_eq!(from_options.data_source, direct.data_source);
        assert_eq!(from_options.database, direct.database);
        assert_eq!(from_options.user_name, direct.user_name);
        assert_eq!(from_options.connect_timeout, direct.connect_timeout);
        assert_eq!(
            from_options.encryption_options.mode,
            direct.encryption_options.mode
        );
        assert_eq!(
            from_options.encryption_options.trust_server_certificate,
            direct.encryption_options.trust_server_certificate
        );
    }

    #[test]
    fn parses_a_full_url() {
        let options = MssqlConnectOptions::parse(URL).expect("the URL must parse");

        assert_eq!(options.host(), "db");
        assert_eq!(options.port(), 1433);
        assert_eq!(options.username, "sa");
        assert_eq!(options.password, "Password1!");
        assert_eq!(options.database(), "testdb");
        assert_eq!(options.encryption, EncryptionSetting::On);
        assert!(options.trust_server_certificate);
        assert_eq!(options.data_source(), "tcp:db,1433");
    }

    #[test]
    fn the_port_defaults_when_the_url_names_none() {
        let options =
            MssqlConnectOptions::parse("mssql://sa:pw@localhost/mydb").expect("the URL must parse");

        assert_eq!(options.port(), DEFAULT_PORT);
        assert_eq!(options.data_source(), "tcp:localhost,1433");
    }

    #[test]
    fn a_url_without_a_path_has_an_empty_database() {
        let options =
            MssqlConnectOptions::parse("mssql://sa:pw@localhost:1433").expect("the URL must parse");

        assert_eq!(options.database(), "");
    }

    #[test]
    fn the_database_parameter_overrides_the_path() {
        let options = MssqlConnectOptions::parse("mssql://sa:pw@localhost/one?database=two")
            .expect("the URL must parse");

        assert_eq!(options.database(), "two");
    }

    #[test]
    fn an_ipv6_host_keeps_its_brackets_in_the_data_source() {
        let options = MssqlConnectOptions::parse("mssql://sa:pw@[::1]:1433/mydb")
            .expect("the URL must parse");

        // `ConnectionUrl` strips the brackets from the host; the data source has
        // to put them back so the port separator stays unambiguous.
        assert_eq!(options.host(), "::1");
        assert_eq!(options.data_source(), "tcp:[::1],1433");
    }

    #[test]
    fn percent_escapes_are_decoded() {
        let options = MssqlConnectOptions::parse("mssql://sa%20user:p%40ss@localhost/my%20db")
            .expect("the URL must parse");

        assert_eq!(options.username, "sa user");
        assert_eq!(options.password, "p@ss");
        assert_eq!(options.database(), "my db");
    }

    #[test]
    fn encryption_accepts_every_spelling() {
        let modes = [
            ("on", EncryptionSetting::On),
            ("true", EncryptionSetting::On),
            ("yes", EncryptionSetting::On),
            ("1", EncryptionSetting::On),
            ("required", EncryptionSetting::Required),
            ("Strict", EncryptionSetting::Strict),
            ("off", EncryptionSetting::PreferOff),
            ("prefer-off", EncryptionSetting::PreferOff),
            ("prefer_off", EncryptionSetting::PreferOff),
            ("false", EncryptionSetting::PreferOff),
            ("no", EncryptionSetting::PreferOff),
            ("0", EncryptionSetting::PreferOff),
        ];

        for (value, expected) in modes {
            let options =
                MssqlConnectOptions::parse(&format!("mssql://sa:pw@db/x?encrypt={value}"))
                    .unwrap_or_else(|error| panic!("`{value}` must be accepted: {error}"));
            assert_eq!(options.encryption, expected, "for `{value}`");
        }

        let error = MssqlConnectOptions::parse("mssql://sa:pw@db/x?encrypt=sometimes")
            .expect_err("an unknown mode must be rejected");
        assert!(
            error.to_string().contains("`encrypt` must be one of"),
            "{error}"
        );
    }

    #[test]
    fn trust_certificate_accepts_booleans() {
        for value in ["true", "yes", "1"] {
            let options = MssqlConnectOptions::parse(&format!(
                "mssql://sa:pw@db/x?trust_certificate={value}"
            ))
            .expect("the URL must parse");
            assert!(options.trust_server_certificate, "for `{value}`");
        }

        for value in ["false", "no", "0"] {
            let options = MssqlConnectOptions::parse(&format!(
                "mssql://sa:pw@db/x?trust_certificate={value}"
            ))
            .expect("the URL must parse");
            assert!(!options.trust_server_certificate, "for `{value}`");
        }

        let error = MssqlConnectOptions::parse("mssql://sa:pw@db/x?trust_certificate=maybe")
            .expect_err("an unknown boolean must be rejected");
        assert!(error.to_string().contains("`trust_certificate`"), "{error}");
    }

    #[test]
    fn the_remaining_parameters_reach_the_client_context() {
        let options = MssqlConnectOptions::parse(
            "mssql://sa:pw@db:1433/x?server_certificate=%2Ftls%2Fserver.pem\
             &host_name_in_cert=sql.example.com&application_name=toasty&connect_timeout=42",
        )
        .expect("the URL must parse");

        assert_eq!(
            options.server_certificate,
            Some(PathBuf::from("/tls/server.pem"))
        );
        assert_eq!(
            options.host_name_in_cert.as_deref(),
            Some("sql.example.com")
        );
        assert_eq!(options.application_name.as_deref(), Some("toasty"));
        assert_eq!(options.connect_timeout, Some(Duration::from_secs(42)));

        let context = options.to_client_context();
        assert_eq!(context.application_name, "toasty");
        assert_eq!(context.connect_timeout, 42);
        assert_eq!(
            context.encryption_options.server_certificate,
            Some(PathBuf::from("/tls/server.pem"))
        );
        assert_eq!(
            context.encryption_options.host_name_in_cert.as_deref(),
            Some("sql.example.com")
        );
    }

    #[test]
    fn a_bad_connect_timeout_is_rejected() {
        let error = MssqlConnectOptions::parse("mssql://sa:pw@db/x?connect_timeout=soon")
            .expect_err("a non-numeric timeout must be rejected");

        assert!(error.to_string().contains("`connect_timeout`"), "{error}");
    }

    #[test]
    fn unknown_parameters_are_ignored() {
        let options = MssqlConnectOptions::parse(
            "mssql://sa:pw@db/x?sslmode=require&applicationIntent=ReadOnly",
        )
        .expect("an unknown parameter must not fail the parse");

        assert_eq!(options.database(), "x");
    }

    #[test]
    fn another_scheme_is_rejected() {
        let error = MssqlConnectOptions::parse("postgresql://sa:pw@db:1433/x")
            .expect_err("another scheme must be rejected");

        assert!(error.to_string().contains("`mssql` scheme"), "{error}");
        assert!(!error.to_string().contains("pw"), "{error}");
    }

    #[test]
    fn a_missing_host_is_rejected() {
        let error = MssqlConnectOptions::parse("mssql://")
            .expect_err("a URL without a host must be rejected");

        assert!(error.to_string().contains("missing a host"), "{error}");
    }

    #[test]
    fn a_non_numeric_port_is_rejected() {
        let error = MssqlConnectOptions::parse("mssql://sa:pw@db:http/x")
            .expect_err("a non-numeric port must be rejected");

        assert!(error.to_string().contains("invalid authority"), "{error}");
    }

    #[test]
    fn parse_errors_do_not_print_the_password() {
        // Every failure that can be reached with an authority present has to
        // redact it, because the message is shown to whoever wrote the URL.
        let url = "mssql://sa:secret@db:1433/x?encrypt=sometimes&trust_certificate=maybe";
        let error = MssqlConnectOptions::parse(url).expect_err("the mode must be rejected");

        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(
            error.to_string().contains("mssql://sa:***@db:1433"),
            "{error}"
        );
    }

    #[test]
    fn debug_does_not_print_the_password() {
        let options = MssqlConnectOptions::parse(URL).expect("the URL must parse");
        let rendered = format!("{options:?}");

        assert!(!rendered.contains("Password1!"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn with_database_keeps_ever_thing_else() {
        let options = MssqlConnectOptions::parse(URL)
            .expect("the URL must parse")
            .with_database("other");

        assert_eq!(options.database(), "other");
        assert_eq!(options.host(), "db");
        assert_eq!(options.username, "sa");
        assert_eq!(options.password, "Password1!");
        assert!(options.trust_server_certificate);
    }

    #[test]
    fn from_str_matches_parse() {
        let options: MssqlConnectOptions = URL.parse().expect("the URL must parse from_str");

        assert_eq!(options.data_source(), "tcp:db,1433");
        assert_eq!(options.database(), "testdb");
    }
}

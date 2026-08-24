use anyhow::{bail, Context, Result};
use argon2::{password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString}, Argon2};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use lettre::{message::Mailbox, transport::smtp::authentication::Credentials, Message, SmtpTransport, Transport};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::time::Duration;

const DEFAULT_SMTP_TIMEOUT_SECONDS: u64 = 30;
const MIN_SMTP_TIMEOUT_SECONDS: u64 = 1;
const MAX_SMTP_TIMEOUT_SECONDS: u64 = 120;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmtpSecurity {
    StartTls,
    Tls,
    None,
}

impl SmtpSecurity {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(Self::StartTls),
            Some(value) if value.eq_ignore_ascii_case("starttls") => Ok(Self::StartTls),
            Some(value)
                if value.eq_ignore_ascii_case("tls")
                    || value.eq_ignore_ascii_case("implicit")
                    || value.eq_ignore_ascii_case("smtps") =>
            {
                Ok(Self::Tls)
            }
            Some(value) if value.eq_ignore_ascii_case("none") => Ok(Self::None),
            Some(_) => bail!(
                "SMTP_SECURITY must be one of: starttls, tls (or implicit), none"
            ),
        }
    }

    fn mode(self) -> &'static str {
        match self {
            Self::StartTls => "starttls",
            Self::Tls => "tls",
            Self::None => "none",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::StartTls => 587,
            Self::Tls => 465,
            Self::None => 25,
        }
    }
}

fn parse_smtp_timeout_seconds(value: Option<&str>) -> Result<u64> {
    let seconds = match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<u64>()
            .context("SMTP_TIMEOUT_SECONDS must be an integer")?,
        None => DEFAULT_SMTP_TIMEOUT_SECONDS,
    };
    if !(MIN_SMTP_TIMEOUT_SECONDS..=MAX_SMTP_TIMEOUT_SECONDS).contains(&seconds) {
        bail!(
            "SMTP_TIMEOUT_SECONDS must be between {MIN_SMTP_TIMEOUT_SECONDS} and {MAX_SMTP_TIMEOUT_SECONDS}"
        );
    }
    Ok(seconds)
}

#[derive(Clone)]
pub struct PasswordPolicy {
    pub minimum_length: usize,
    pub maximum_length: usize,
}

impl PasswordPolicy {
    pub fn from_env() -> Result<Self> {
        let minimum_length=std::env::var("PASSWORD_MIN_LENGTH").unwrap_or_else(|_|"14".into()).parse::<usize>().context("PASSWORD_MIN_LENGTH must be an integer")?;
        let maximum_length=std::env::var("PASSWORD_MAX_LENGTH").unwrap_or_else(|_|"256".into()).parse::<usize>().context("PASSWORD_MAX_LENGTH must be an integer")?;
        if minimum_length<12||maximum_length<minimum_length||maximum_length>1024{bail!("password length policy must satisfy 12 <= minimum <= maximum <= 1024");}
        Ok(Self{minimum_length,maximum_length})
    }

    pub fn validate(&self,password:&str,username:&str,email:&str)->Result<()> {
        let count=password.chars().count();
        if count<self.minimum_length||count>self.maximum_length{bail!("password must contain between {} and {} characters",self.minimum_length,self.maximum_length);}
        if password.chars().any(char::is_control){bail!("password cannot contain control characters");}
        let classes=[password.chars().any(char::is_lowercase),password.chars().any(char::is_uppercase),password.chars().any(|c|c.is_ascii_digit()),password.chars().any(|c|!c.is_alphanumeric())];
        if classes.into_iter().filter(|value|*value).count()<3{bail!("password must include at least three of: lowercase, uppercase, number, symbol");}
        let lowered=password.to_lowercase();
        let username=username.to_lowercase();
        let email_local=email.split('@').next().unwrap_or_default().to_lowercase();
        if username.len()>=3&&lowered.contains(&username){bail!("password cannot contain the username");}
        if email_local.len()>=3&&lowered.contains(&email_local){bail!("password cannot contain the email name");}
        Ok(())
    }
}

pub fn normalize_username(value:&str)->Result<String>{
    let value=value.trim().to_ascii_lowercase();
    if !(3..=64).contains(&value.len()){bail!("username must contain 3 to 64 characters");}
    if !value.chars().all(|c|c.is_ascii_alphanumeric()||matches!(c,'.'|'_'|'-')){bail!("username may contain only letters, numbers, periods, underscores, and hyphens");}
    if !value.chars().next().is_some_and(|c|c.is_ascii_alphanumeric()){bail!("username must start with a letter or number");}
    Ok(value)
}

pub fn normalize_email(value:&str)->Result<String>{
    let value=value.trim().to_ascii_lowercase();
    let Some((local,domain))=value.split_once('@') else{bail!("a valid email address is required");};
    if local.is_empty()||domain.is_empty()||!domain.contains('.')||value.len()>254||value.chars().any(char::is_whitespace){bail!("a valid email address is required");}
    Ok(value)
}

pub fn hash_password(password:&str)->Result<String>{
    let salt=SaltString::generate(&mut OsRng);
    Ok(Argon2::default().hash_password(password.as_bytes(),&salt).map_err(|error|anyhow::anyhow!("password hashing failed: {error}"))?.to_string())
}

pub fn verify_password(password:&str,encoded:&str)->bool{
    PasswordHash::new(encoded).ok().is_some_and(|hash|Argon2::default().verify_password(password.as_bytes(),&hash).is_ok())
}

pub fn generate_secret()->String{let mut bytes=[0u8;32];OsRng.fill_bytes(&mut bytes);URL_SAFE_NO_PAD.encode(bytes)}
pub fn sha256(value:&str)->String{hex::encode(Sha256::digest(value.as_bytes()))}
pub fn constant_time_eq(left:&[u8],right:&[u8])->bool{
    if left.len()!=right.len(){return false;}
    left.iter().zip(right).fold(0u8,|difference,(a,b)|difference|(a^b))==0
}

#[derive(Clone)]
pub struct EmailSettings {
    host:String,
    port:u16,
    security:SmtpSecurity,
    timeout_seconds:u64,
    username:Option<String>,
    password:Option<String>,
    from:Mailbox,
    public_url:String,
}

impl EmailSettings {
    pub fn from_env(required:bool)->Result<Option<Self>>{
        let host=std::env::var("SMTP_HOST").ok().filter(|v|!v.trim().is_empty());
        if host.is_none(){if required{bail!("SMTP_HOST is required when AUTH_MODE=internal_accounts");}return Ok(None);}
        let from=std::env::var("SMTP_FROM").context("SMTP_FROM is required when SMTP_HOST is set")?.parse::<Mailbox>().context("SMTP_FROM is not a valid mailbox")?;
        let public_url=std::env::var("APP_PUBLIC_URL").context("APP_PUBLIC_URL is required when SMTP_HOST is set")?.trim_end_matches('/').to_owned();
        if !public_url.starts_with("https://")&&!public_url.starts_with("http://localhost")&&!public_url.starts_with("http://127.0.0.1"){bail!("APP_PUBLIC_URL must use HTTPS except on loopback development hosts");}
        let security_value=std::env::var("SMTP_SECURITY").ok();
        let security=SmtpSecurity::parse(security_value.as_deref())?;
        let port=std::env::var("SMTP_PORT").ok().filter(|value|!value.trim().is_empty()).map(|value|value.parse::<u16>().context("SMTP_PORT must be a valid port")).transpose()?.unwrap_or_else(||security.default_port());
        if port==0{bail!("SMTP_PORT must be between 1 and 65535");}
        let timeout_value=std::env::var("SMTP_TIMEOUT_SECONDS").ok();
        let timeout_seconds=parse_smtp_timeout_seconds(timeout_value.as_deref())?;
        let username=std::env::var("SMTP_USERNAME").ok().filter(|v|!v.is_empty());
        let password=std::env::var("SMTP_PASSWORD").ok().filter(|v|!v.is_empty());
        if username.is_some()!=password.is_some(){bail!("SMTP_USERNAME and SMTP_PASSWORD must be configured together");}
        Ok(Some(Self{host:host.unwrap().trim().to_owned(),port,security,timeout_seconds,username,password,from,public_url}))
    }

    fn send(&self,to:&str,subject:&str,body:String)->Result<()> {
        let message=Message::builder().from(self.from.clone()).to(to.parse::<Mailbox>().context("recipient email is invalid")?).subject(subject).body(body)?;
        let builder=match self.security{
            SmtpSecurity::StartTls=>SmtpTransport::starttls_relay(&self.host)?,
            SmtpSecurity::Tls=>SmtpTransport::relay(&self.host)?,
            SmtpSecurity::None=>SmtpTransport::builder_dangerous(&self.host),
        };
        let mut builder=builder.port(self.port).timeout(Some(Duration::from_secs(self.timeout_seconds)));
        if let (Some(username),Some(password))=(&self.username,&self.password){builder=builder.credentials(Credentials::new(username.clone(),password.clone()));}
        builder.build().send(&message).context("SMTP delivery failed")?;
        Ok(())
    }

    pub fn delivery_mode(&self)->&'static str{self.security.mode()}

    pub fn send_new_account(&self,to:&str,username:&str,temp_password:&str)->Result<()> {
        self.send(to,"Your Grantspace account",format!("An administrator created your Grantspace account.\n\nUsername: {username}\nTemporary password: {temp_password}\nLogin: {}/login\n\nYou must choose a new password immediately after your first login. Do not forward this message.",self.public_url))
    }

    pub fn send_password_reset(&self,to:&str,raw_token:&str,expires_minutes:u64)->Result<()> {
        self.send(to,"Reset your Grantspace password",format!("A password reset was requested for your Grantspace account.\n\nOpen this single-use link within {expires_minutes} minutes:\n{}/password-reset?token={}\n\nIf you did not request this, you can ignore this message.",self.public_url,raw_token))
    }

    pub fn send_project_invite(&self,to:&str,project_title:&str,role:&str,raw_token:&str,expires_days:u32)->Result<()> {
        self.send(to,"You were invited to a Grantspace project",format!("You were invited to the Grantspace project \"{project_title}\" with the role {role}.\n\nSign in with the Grantspace account matching this email address, then accept the single-use invitation within {expires_days} day(s):\n{}/invite?token={}\n\nIf you do not yet have an account, contact the Grantspace administrator who invited you. Do not forward this link.",self.public_url,raw_token))
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn password_hashes_verify_and_policy_rejects_identity_content()->Result<()> {
        let policy=PasswordPolicy{minimum_length:14,maximum_length:256};
        let password="G7!violet-forest-window";
        policy.validate(password,"researcher","person@example.org")?;
        let encoded=hash_password(password)?;
        assert!(verify_password(password,&encoded));
        assert!(!verify_password("wrong-password",&encoded));
        assert!(policy.validate("Researcher!Pass123","researcher","person@example.org").is_err());
        assert!(policy.validate("short!A1","researcher","person@example.org").is_err());
        Ok(())
    }

    #[test]
    fn gateway_secrets_use_constant_time_content_comparison(){
        assert!(constant_time_eq(b"0123456789abcdef",b"0123456789abcdef"));
        assert!(!constant_time_eq(b"0123456789abcdef",b"0123456789abcdee"));
        assert!(!constant_time_eq(b"short",b"longer"));
    }

    #[test]
    fn usernames_and_emails_are_normalized_and_validated()->Result<()> {
        assert_eq!(normalize_username(" Admin.User ")?,"admin.user");
        assert_eq!(normalize_email(" Person@Example.ORG ")?,"person@example.org");
        assert!(normalize_username("invalid user").is_err());
        assert!(normalize_email("missing-domain").is_err());
        Ok(())
    }

    #[test]
    fn smtp_security_modes_and_defaults_are_explicit()->Result<()> {
        assert_eq!(SmtpSecurity::parse(None)?,SmtpSecurity::StartTls);
        assert_eq!(SmtpSecurity::parse(Some("STARTTLS"))?,SmtpSecurity::StartTls);
        assert_eq!(SmtpSecurity::parse(Some("implicit"))?,SmtpSecurity::Tls);
        assert_eq!(SmtpSecurity::parse(Some("smtps"))?,SmtpSecurity::Tls);
        assert_eq!(SmtpSecurity::parse(Some("none"))?,SmtpSecurity::None);
        assert_eq!(SmtpSecurity::StartTls.default_port(),587);
        assert_eq!(SmtpSecurity::Tls.default_port(),465);
        assert_eq!(SmtpSecurity::None.default_port(),25);
        assert!(SmtpSecurity::parse(Some("opportunistic")).is_err());
        Ok(())
    }

    #[test]
    fn smtp_timeout_is_bounded()->Result<()> {
        assert_eq!(parse_smtp_timeout_seconds(None)?,DEFAULT_SMTP_TIMEOUT_SECONDS);
        assert_eq!(parse_smtp_timeout_seconds(Some("1"))?,1);
        assert_eq!(parse_smtp_timeout_seconds(Some("120"))?,120);
        assert!(parse_smtp_timeout_seconds(Some("0")).is_err());
        assert!(parse_smtp_timeout_seconds(Some("121")).is_err());
        assert!(parse_smtp_timeout_seconds(Some("not-a-number")).is_err());
        Ok(())
    }
}

use anyhow::{bail, Context, Result};
use argon2::{password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString}, Argon2};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use lettre::{message::Mailbox, transport::smtp::authentication::Credentials, Message, SmtpTransport, Transport};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};

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
        let port=std::env::var("SMTP_PORT").unwrap_or_else(|_|"587".into()).parse::<u16>().context("SMTP_PORT must be a valid port")?;
        let username=std::env::var("SMTP_USERNAME").ok().filter(|v|!v.is_empty());
        let password=std::env::var("SMTP_PASSWORD").ok().filter(|v|!v.is_empty());
        if username.is_some()!=password.is_some(){bail!("SMTP_USERNAME and SMTP_PASSWORD must be configured together");}
        Ok(Some(Self{host:host.unwrap(),port,username,password,from,public_url}))
    }

    fn send(&self,to:&str,subject:&str,body:String)->Result<()> {
        let message=Message::builder().from(self.from.clone()).to(to.parse::<Mailbox>().context("recipient email is invalid")?).subject(subject).body(body)?;
        let mut builder=SmtpTransport::relay(&self.host)?.port(self.port);
        if let (Some(username),Some(password))=(&self.username,&self.password){builder=builder.credentials(Credentials::new(username.clone(),password.clone()));}
        builder.build().send(&message).context("SMTP delivery failed")?;
        Ok(())
    }

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
}

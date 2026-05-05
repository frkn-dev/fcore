use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::{
    fs::{File, OpenOptions},
    io::AsyncWriteExt,
    io::{AsyncBufReadExt, BufReader},
    sync::RwLock,
};

use super::config::SmtpConfig;

type HmacSha256 = Hmac<Sha256>;

use lettre::{
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
        AsyncSmtpTransport,
    },
    AsyncTransport, Message, Tokio1Executor,
};

#[derive(Clone)]
pub struct EmailStore {
    pub store: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    smtp: SmtpConfig,
    mailer: Arc<AsyncSmtpTransport<Tokio1Executor>>,
}

impl EmailStore {
    pub fn new(smtp: SmtpConfig) -> Self {
        let mailer = EmailStore::build_mailer(&smtp);

        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
            smtp,
            mailer: Arc::new(mailer),
        }
    }

    fn hmac_email(&self, email: &str) -> String {
        let secret = &self.smtp.email_sign_token;
        let mut mac = HmacSha256::new_from_slice(&secret).unwrap();
        mac.update(email.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    pub async fn check_email_hmac(&self, email: &str) -> bool {
        let email_hmac = self.hmac_email(email);
        let store = self.store.read().await;
        store.contains_key(&email_hmac)
    }

    pub async fn save_trial_hmac(
        &self,
        email: &str,
        sub_id: &uuid::Uuid,
        time: &DateTime<Utc>,
        ref_by: &str,
    ) -> std::io::Result<()> {
        let email_hmac = self.hmac_email(email);

        {
            let mut store = self.store.write().await;
            store.insert(email_hmac.clone(), *time);
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.smtp.email_file)
            .await;

        let line = format!(
            "{},{},{},{}\n",
            time.to_rfc3339(),
            email_hmac,
            sub_id,
            ref_by
        );

        file?.write_all(line.as_bytes()).await?;

        Ok(())
    }

    pub async fn load_trials(&self) -> std::io::Result<()> {
        let file = match File::open(&self.smtp.email_file).await {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };

        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let mut map = HashMap::new();

        while let Some(line) = lines.next_line().await? {
            let parts: Vec<&str> = line.split(',').collect();

            if parts.len() >= 2 {
                if let Ok(ts) = parts[0].parse::<DateTime<Utc>>() {
                    map.insert(parts[1].to_string(), ts);
                }
            }
        }

        let mut store = self.store.write().await;
        *store = map;

        Ok(())
    }

    fn build_mailer(smtp: &SmtpConfig) -> AsyncSmtpTransport<Tokio1Executor> {
        let creds = Credentials::new(smtp.username.clone(), smtp.password.clone());

        let tls = TlsParameters::new(smtp.server.clone()).expect("TLS params failed");

        AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.server)
            .unwrap()
            .credentials(creds)
            .port(smtp.port)
            .tls(Tls::Required(tls))
            .build()
    }

    pub async fn send_email_background(&self, to: String, sub_id: uuid::Uuid) {
        let mailer = self.mailer.clone();
        let web_host = self.smtp.company_website.clone();
        let from = self.smtp.from.clone();
        let title = self.smtp.title.clone();
        let company_name = self.smtp.company_name.clone();
        let support = self.smtp.support.clone();

        tokio::spawn(async move {
            let html_body = format!(
                r#"
            <!DOCTYPE html>
            <html>
            <head>
              <meta charset="UTF-8">
              <title>{title}</title>
            </head>
            <body style="margin:0;padding:0;background:#0b0d12;font-family:Arial,sans-serif;">

              <table width="100%" cellpadding="0" cellspacing="0" style="background:#0b0d12;padding:40px 0;">
                <tr>
                  <td align="center">

                    <table width="600" cellpadding="0" cellspacing="0" style="background:#121621;border-radius:16px;padding:32px;color:#e6e8ef;">

                      <tr>
                        <td style="text-align:center;">
                          <h1 style="margin:0;color:#5b7cfa;">{title}</h1>
                          <p style="color:#9aa1b2;margin-top:8px;">
                            Твоя подписка для тест-драйва успешно создана
                          </p>
                        </td>
                      </tr>

                      <tr>
                        <td style="padding:20px 0;text-align:center;">
                          <div style="font-size:12px;color:#9aa1b2;">Subscription ID</div>
                          <div style="font-family:monospace;font-size:14px;word-break:break-all;">
                            {sub_id}
                          </div>
                        </td>
                      </tr>

                      <tr>
                        <td align="center" style="padding:20px 0;">

                          <a href="{web_host}/subscription?id={sub_id}"
                             style="
                              display:inline-block;
                              padding:14px 24px;
                              background:linear-gradient(90deg,#5b7cfa,#22d3ee);
                              color:#fff;
                              text-decoration:none;
                              border-radius:12px;
                              font-weight:bold;
                             ">
                            Открыть подписку
                          </a>

                        </td>
                      </tr>

                      <tr>
                        <td style="text-align:center;color:#9aa1b2;font-size:12px;padding-top:16px;">
                          Если кнопка не работает — скопируй ссылку:<br>
                          <span style="color:#5b7cfa;">
                            {web_host}/subscription?id={sub_id}
                          </span>
                        </td>
                      </tr>

                      <tr>
                        <td style="text-align:center;padding-top:24px;font-size:11px;color:#6b7280;">
                          {company_name} • <a href="{support}">Поддержка</a>
                        </td>
                      </tr>

                    </table>

                  </td>
                </tr>
              </table>

            </body>
            </html>
            "#,
                web_host = web_host,
                sub_id = sub_id,
                title = title,
                company_name = company_name,
                support = support,
            );

            let msg = match Message::builder()
                .from(from.parse().unwrap())
                .to(to.parse().unwrap())
                .subject(title)
                .header(lettre::message::header::ContentType::TEXT_HTML)
                .body(html_body)
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("Email build error: {}", e);
                    return;
                }
            };

            for i in 0..3 {
                match mailer.send(msg.clone()).await {
                    Ok(_) => return,
                    Err(e) => {
                        tracing::error!("SMTP attempt {i} failed: {e:?}");
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        });
    }
}

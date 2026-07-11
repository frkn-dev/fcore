use lettre::{
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
        AsyncSmtpTransport,
    },
    AsyncTransport, Message, Tokio1Executor,
};

use super::config::SmtpConfig;

#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
    title: String,
    company_name: String,
    support: String,
    company_website: String,
}

impl Mailer {
    pub fn new(config: &SmtpConfig) -> Self {
        let creds = Credentials::new(config.username.clone(), config.password.clone());
        let tls = TlsParameters::new(config.server.clone()).expect("TLS params failed");
        let transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&config.server)
            .unwrap()
            .credentials(creds)
            .port(config.port)
            .tls(Tls::Required(tls))
            .build();

        Self {
            transport,
            from: config.from.clone(),
            title: config.title.clone(),
            company_name: config.company_name.clone(),
            support: config.support.clone(),
            company_website: config.company_website.clone(),
        }
    }

    pub fn send_welcome_email(
        &self,
        to: String,
        sub_id: uuid::Uuid,
        lang: Option<String>,
    ) {
        let transport = self.transport.clone();
        let from = self.from.clone();
        let title = self.title.clone();
        let company_name = self.company_name.clone();
        let support = self.support.clone();
        let company_website = self.company_website.clone();

        tokio::spawn(async move {
            let is_en = lang.as_deref().map(|s| s.to_lowercase()).as_deref() == Some("en");

            let email_title = if is_en {
                "Subscription created".to_string()
            } else {
                title
            };
            let subtitle = if is_en {
                "Your subscription has been created"
            } else {
                "Твоя подписка успешно создана"
            };
            let button_text = if is_en { "Open subscription" } else { "Открыть подписку" };
            let fallback_text = if is_en {
                "If the button doesn't work, copy the link:"
            } else {
                "Если кнопка не работает — скопируй ссылку:"
            };
            let support_label = if is_en { "Support" } else { "Поддержка" };

            let html_body = format!(
                r#"
            <!DOCTYPE html>
            <html>
            <head>
              <meta charset="UTF-8">
              <title>{email_title}</title>
            </head>
            <body style="margin:0;padding:0;background:#0b0d12;font-family:Arial,sans-serif;">
              <table width="100%" cellpadding="0" cellspacing="0" style="background:#0b0d12;padding:40px 0;">
                <tr>
                  <td align="center">
                    <table width="600" cellpadding="0" cellspacing="0" style="background:#121621;border-radius:16px;padding:32px;color:#e6e8ef;">
                      <tr>
                        <td style="text-align:center;">
                          <h1 style="margin:0;color:#5b7cfa;">{email_title}</h1>
                          <p style="color:#9aa1b2;margin-top:8px;">{subtitle}</p>
                        </td>
                      </tr>
                      <tr>
                        <td style="padding:20px 0;text-align:center;">
                          <div style="font-size:12px;color:#9aa1b2;">Subscription ID</div>
                          <div style="font-family:monospace;font-size:14px;word-break:break-all;">{sub_id}</div>
                        </td>
                      </tr>
                      <tr>
                        <td align="center" style="padding:20px 0;">
                          <a href="{web_host}/subscription?id={sub_id}"
                             style="display:inline-block;padding:14px 24px;background:linear-gradient(90deg,#5b7cfa,#22d3ee);color:#fff;text-decoration:none;border-radius:12px;font-weight:bold;">{button_text}</a>
                        </td>
                      </tr>
                      <tr>
                        <td style="text-align:center;color:#9aa1b2;font-size:12px;padding-top:16px;">
                          {fallback_text}<br>
                          <span style="color:#5b7cfa;">{web_host}/subscription?id={sub_id}</span>
                        </td>
                      </tr>
                      <tr>
                        <td style="text-align:center;padding-top:24px;font-size:11px;color:#6b7280;">
                          {company_name} • <a href="{support}">{support_label}</a>
                        </td>
                      </tr>
                    </table>
                  </td>
                </tr>
              </table>
            </body>
            </html>
            "#,
                web_host = company_website,
                sub_id = sub_id,
                email_title = email_title,
                subtitle = subtitle,
                button_text = button_text,
                fallback_text = fallback_text,
                company_name = company_name,
                support = support,
                support_label = support_label,
            );

            let msg = match Message::builder()
                .from(from.parse().unwrap())
                .to(to.parse().unwrap())
                .subject(email_title)
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
                match transport.send(msg.clone()).await {
                    Ok(_) => return,
                    Err(e) => {
                        tracing::error!("SMTP attempt {} failed: {:?}", i, e);
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        });
    }
}

# Trojan inbound

Trojan uses a TCP listener with TLS and a server certificate. The IP certificate already provisioned on each node can be used if clients support IP address certificates. Give each node its own certificate and private key; do not copy another node's key.

Example Xray inbound (replace the certificate paths with paths visible inside Xray):

```json
{
  "tag": "Trojan",
  "listen": "0.0.0.0",
  "port": 8445,
  "protocol": "trojan",
  "settings": { "clients": [] },
  "streamSettings": {
    "network": "tcp",
    "security": "tls",
    "tlsSettings": {
      "certificates": [{
        "certificateFile": "/etc/letsencrypt/live/NODE-IP/fullchain.pem",
        "keyFile": "/etc/letsencrypt/live/NODE-IP/privkey.pem"
      }]
    }
  }
}
```

Mount the entire certificate directory read-only into Xray so its `live` symlinks work. Port 8445/TCP must be reachable. Xray 26.9.9 reloads certificates from files automatically (normally within an hour) when one-time loading is disabled; mount the entire directory so renewed symlinks remain visible. Verify this behaviour after deployment. If Xray is restarted, also restart the node agent to restore its dynamic users.

Apply migration `0008_trojan.sql` before deploying the new API, then deploy compatible API and node binaries together before enabling Trojan inbounds. The new `Trojan` tag is appended to the serialized tag enum; do not reorder variants. Verify certificate renewal, user creation, traffic and revocation before relying on the service.

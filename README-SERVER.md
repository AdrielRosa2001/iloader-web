# iloader headless (servidor + interface web)

Versão do [iloader](https://github.com/nab138/iloader), de **nab138**, que roda num servidor Linux sem interface gráfica e é usada pelo navegador de outra máquina da rede.

> Projeto **não oficial**, sem afiliação nem endosso do iloader. O código original é MIT (`LICENSE`); o nome e os logos "iloader" seguem o `LICENSE-BRANDING`. Site oficial: <https://iloader.app>.

## Como funciona

```
navegador (notebook) ──HTTPS/túnel──▶ iloader-server (Ubuntu Server) ──usbmuxd──▶ iPhone (USB)
```

- `crates/iloader-core`: a lógica do iloader (login Apple + 2FA, pairing, instalação), sem Tauri.
- `crates/iloader-server`: servidor HTTP/WebSocket (axum) que serve a interface e a API.
- `src/`: a mesma interface React do app desktop. Os módulos `@tauri-apps/*` são trocados por `src/platform/` via alias no `vite.config.ts`.
- `src-tauri/`: app desktop original, mantido sem alterações para facilitar merges do upstream. Ele não faz parte do build headless.

## Compilar

Requisitos: Rust 1.85 ou mais novo (`rustup`), `bun` (ou Node 20+ com npm), `gcc`/`cc` (`build-essential`). Não precisa de cmake, GTK, WebKit, dbus nem pkg-config.

```sh
cargo build --release -p iloader-server   # binário em target/release/iloader-server
bun install && bun run build              # interface em dist/
```

Dá para compilar em outra máquina x86_64 e copiar `target/release/iloader-server` + `dist/` para o servidor.

## Instalar no Ubuntu Server

```sh
sudo ./deploy/install.sh
```

O script:

1. instala `usbmuxd` e cria o usuário de sistema `iloader`;
2. copia o binário e `dist/` para `/opt/iloader-server`;
3. cria `/etc/iloader-server/env` a partir de `deploy/env.example` (se ainda não existir);
4. instala e inicia a unit systemd `iloader-server` (`Restart=on-failure`, usuário sem privilégios, `ProtectSystem=strict`, `NoNewPrivileges`, `UMask=0077`).

Pegue o token de acesso gerado na primeira execução:

```sh
sudo cat /var/lib/iloader-server/access_token
```

Logs: `journalctl -u iloader-server -f` e `/var/lib/iloader-server/logs/` (rotação diária, 2 arquivos).

### iPhone

1. Ligue o iPhone por USB no servidor e confira: `idevice_id -l` (pacote `libimobiledevice-utils`) deve listar o UDID.
2. Desbloqueie o aparelho e toque em **Confiar** quando aparecer.
3. O `usbmuxd` do Ubuntu é ativado pelo udev quando o aparelho é conectado e cria `/var/run/usbmuxd` (modo 0666). Se a sua distro restringir o socket, adicione o grupo dono dele em `SupplementaryGroups=` na unit.

**Opção B, não testada:** iPhone ligado em outra máquina. Exponha o usbmuxd dela por TCP (por exemplo `socat TCP-LISTEN:27015,reuseaddr,fork UNIX-CONNECT:/var/run/usbmuxd`, de preferência dentro de um túnel SSH) e defina `USBMUXD_SOCKET_ADDRESS=IP:27015` no `env`. Tem que ser IP, não nome de host.

## Configuração (`/etc/iloader-server/env`)

| Variável | Padrão | Uso |
| --- | --- | --- |
| `ILOADER_BIND` | `127.0.0.1:8080` | endereço/porta. Fora do loopback, só com TLS |
| `ILOADER_DATA_DIR` | `~/.local/share/iloader-server` | estado (anisette, pairing, contas, logs, token) |
| `ILOADER_TEMP_DIR` | `/tmp/iloader-server` | uploads e IPAs baixados |
| `ILOADER_STATIC_DIR` | `./dist` | interface compilada |
| `ILOADER_TOKEN` / `ILOADER_TOKEN_FILE` | gerado em `<data>/access_token` | token de acesso (mínimo 12 caracteres) |
| `ILOADER_SECRET_KEY_FILE` | — | habilita "salvar credenciais" (ver abaixo) |
| `ILOADER_COOKIE_SECURE` | `0` | `1` quando servido por HTTPS |
| `ILOADER_ALLOWED_ORIGINS` | — | origens extras aceitas (proxy que reescreve o `Host`) |
| `ILOADER_MAX_UPLOAD_MB` | `2048` | tamanho máximo do IPA |
| `ILOADER_UI_LOG_LEVEL` | `debug` | nível dos logs enviados à interface |
| `USBMUXD_SOCKET_ADDRESS` | `/var/run/usbmuxd` | caminho ou `IP:porta` do usbmuxd |

## Credenciais e dados guardados

| Dado | Onde | Proteção |
| --- | --- | --- |
| Senha do Apple ID | `accounts.json` | **não é salva por padrão**. Só com `ILOADER_SECRET_KEY_FILE`: cifrada com ChaCha20-Poly1305, arquivo 0600 |
| Lista de e-mails salvos | `accounts.json` | arquivo 0600 |
| Estado do anisette, certificados, RPPairing | `storage/` | arquivos 0600 numa pasta 0700 (sem cifragem, como o fallback do app desktop) |
| Token de acesso | `access_token` | arquivo 0600 |

Para habilitar o salvamento de senha:

```sh
sudo sh -c 'head -c 32 /dev/urandom | base64 > /etc/iloader-server/secret.key'
sudo chown root:iloader /etc/iloader-server/secret.key && sudo chmod 640 /etc/iloader-server/secret.key
echo 'ILOADER_SECRET_KEY_FILE=/etc/iloader-server/secret.key' | sudo tee -a /etc/iloader-server/env
sudo systemctl restart iloader-server
```

Se a chave mudar ou for perdida, as senhas salvas deixam de abrir e é preciso logar de novo. Na interface, "keyring indisponível" significa que o salvamento está desligado no servidor.

O login no Apple ID (sessão em memória) se perde quando o serviço reinicia. As sessões do navegador também: depois de um restart, a interface pede o token de novo.

## Acesso seguro pela rede

O serviço manipula a **senha do Apple ID** e o **pairing file** (que dá acesso ao iPhone). O que o servidor faz:

- exige o token em todas as rotas `/api` e no WebSocket. O token é trocado por um cookie de sessão `HttpOnly; SameSite=Strict`, ou enviado como `Authorization: Bearer`, e comparado em tempo constante;
- bloqueia o IP por 5 minutos após 5 tentativas de token erradas;
- recusa requisições e WebSockets cujo `Origin` não seja o próprio servidor (sem CORS);
- só aceita IPAs enviados pelo upload, referenciados por um id opaco (nada de caminhos), apaga o arquivo depois da instalação e limpa sobras com mais de 2 horas;
- envia CSP, `X-Frame-Options: DENY`, `nosniff` e `no-referrer`.

**Sem TLS, o token e a senha do Apple ID trafegam em texto na rede.** Use uma das opções:

### Opção 1: túnel SSH (mais simples)

Deixe `ILOADER_BIND=127.0.0.1:8080` e, no notebook:

```sh
ssh -N -L 8080:127.0.0.1:8080 usuario@servidor
```

Abra <http://localhost:8080>.

### Opção 2: reverse proxy com HTTPS

Caddy (certificado interno para a LAN):

```
iloader.lan {
    tls internal
    reverse_proxy 127.0.0.1:8080
}
```

nginx (o `Host` precisa ser repassado e as operações podem demorar minutos):

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host $host;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 900s;
    client_max_body_size 2g;
}
```

Com HTTPS, defina `ILOADER_COOKIE_SECURE=1`.

### Firewall

Se abrir a porta na LAN, libere só o notebook:

```sh
sudo ufw allow from 192.168.0.50 to any port 443 proto tcp
```

## API (resumo)

| Rota | Uso |
| --- | --- |
| `POST /api/auth/login` `{token}` | cria a sessão (cookie) |
| `POST /api/auth/logout` | encerra a sessão |
| `GET /api/auth/session` | 200 se autenticado |
| `POST /api/invoke/:command` | os 22 comandos do app (args em camelCase, como no Tauri) + `saved_accounts` |
| `GET /api/events` (WebSocket) | `{event, payload}`: `operation_*`, `2fa-required`, `max-certs-reached`, `log-record` |
| `POST /api/emit/:event` | respostas `2fa-recieved` e `max-certs-response` |
| `POST /api/upload` | corpo = bytes do IPA → `{id}`, usado como `appPath` em `sideload_operation` |
| `GET /api/pairing/export` | download do pairing file do dispositivo selecionado |
| `GET /api/version` | versão |

Erros vêm como `{ "type": "...", "message": "..." }`. Os status usados: `401` sem autenticação, `409` para `busy` (outra operação em andamento), `not_logged_in` e `no_device_selected`, e `500` para os demais erros do iloader.

Exemplo com curl:

```sh
T=$(sudo cat /var/lib/iloader-server/access_token)
curl -s -X POST -H "Authorization: Bearer $T" http://127.0.0.1:8080/api/invoke/list_devices
```

## Desenvolvimento

```sh
cargo run -p iloader-server            # API em :8080 (dados em ~/.local/share/iloader-server)
bun run dev                            # UI em :1420, com proxy de /api para :8080
cargo test --workspace                 # testes sem hardware
```

## Testes com hardware (a fazer no servidor real)

1. Listar dispositivos e tocar em "Confiar" no iPhone.
2. Selecionar o dispositivo (gera o pairing).
3. Login com Apple ID, incluindo o 2FA pela interface.
4. Instalar SideStore ou um IPA pequeno e colocar o pairing file.
5. Listar e revogar certificados e App IDs.
6. Reiniciar o serviço e logar de novo (com e sem credenciais salvas).

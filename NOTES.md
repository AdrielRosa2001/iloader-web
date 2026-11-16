# NOTES — fork headless (fase 0)

Data: 2026-11-16. Branch: `web-headless`.

## Upstream

- `git ls-remote https://github.com/nab138/iloader` → `main` ainda em `96ed061` (o mesmo commit da análise do PLAN). Nada a incorporar.

## Toolchain

- Rust 1.100.0 instalado via rustup (perfil minimal) em `~/.cargo` — suporta edition 2024.
- `bun` não está instalado; Node 24 + npm funcionam com o `package.json` (o `bun.lock` fica só como referência).
- Não há `pkg-config` nem `libdbus-1-dev` nesta máquina. Isso importa por causa do `keyring` (ver abaixo).

## Verificações nas fontes das dependências

### 1. `UsbmuxdAddr::from_env_var()` (idevice 0.1.68, `src/usbmuxd/mod.rs:120`)

- Lê `USBMUXD_SOCKET_ADDRESS`.
- Em Unix: se o valor contém `:` é tratado como `SocketAddr` TCP (`ip:porta`, **sem** nome de host — tem que ser IP); senão é caminho de socket Unix.
- Sem a variável: `/var/run/usbmuxd` (Unix).
- **Pegadinha:** `UsbmuxdConnection::default()` **ignora** a variável (usa `UsbmuxdAddr::default()`), mas os providers usam `from_env_var()`. No original, listar dispositivos falaria com o socket local e as conexões com o endereço da variável. No core troquei `get_usbmuxd()` para `UsbmuxdAddr::from_env_var()?.connect(0)`, então a opção B (usbmuxd remoto via TCP, ex.: `socat` no notebook) passa a ser coerente. Continua não testada com hardware.

### 2. `keyring` com `linux-native-sync-persistent` em servidor sem GUI

- O feature puxa `dbus-secret-service` → crate `dbus` → `libdbus-sys`, que precisa de `pkg-config` + `libdbus-1-dev` **para compilar**.
- Em runtime precisa de um Secret Service (gnome-keyring/kwallet) numa sessão D-Bus. Num Ubuntu Server sem sessão gráfica não há isso; o `keyutils` (kernel) não persiste entre reinícios.
- **Decisão:** o core não usa `keyring`. `isideload` é usado com `default-features = false` (sem `keyring-storage`). Armazenamento próprio em arquivo (fase 4).

### 3. `FsStorage` (isideload 0.5.2, `src/util/fs_storage.rs`)

- Um arquivo por chave em `<dir>/<chave>`, via `std::fs::write` → permissões pelo `umask` (normalmente 0644). Sem cifragem.
- **Bug:** `delete()` não é sobrescrito; o default do trait grava `""`, e `retrieve_data` devolve `Some(vec![])`. Ou seja, depois de "apagar" o RPPairing, `has_stored_rppairing` continuaria `true`.
- **Decisão:** o core tem seu próprio `FileStorage` (arquivos 0600, pasta 0700, `delete` remove o arquivo, chaves validadas contra path traversal).
- Chave do estado do anisette: `anisette_state` (`ANISETTE_STATE_KEY`).

### 4. Nomes dos argumentos do `invoke`

- O frontend manda camelCase (`appPath`, `bundleId`, `saveCredentials`, `anisetteServer`, `serialNumber`, `appIdId`); o Tauri converte para snake_case. O servidor usa structs com `#[serde(rename_all = "camelCase")]`.
- `invoke("set_selected_device")` é chamado **sem argumentos** em `Settings.tsx` → `device` ausente = `null`. O servidor trata corpo vazio como `{}`.
- Eventos `2fa-recieved`: payload é a string do código. `max-certs-response`: `null` ou lista de seriais.

## Logs e segredos

- Não achei logs de senha/token no `isideload` (os `debug!` de "token" só registram o nome do app). O código do iloader também não loga a senha.
- O fluxo de logs para a UI só vai para WebSockets autenticados.

## Decisões em aberto (seção 8 do PLAN) — padrões adotados

1. iPhone por USB no servidor (A). B fica possível via `USBMUXD_SOCKET_ADDRESS=ip:porta`, sem garantia.
2. App desktop: `src-tauri/` fica intocado e **fora** do workspace (facilita merge do upstream), mas não é mantido.
3. Senha do Apple ID: **não** salva por padrão. Só é possível salvar se houver chave configurada (`ILOADER_SECRET_KEY_FILE`); aí é cifrada (ChaCha20-Poly1305) em arquivo 0600.
4. Acesso: token obrigatório + recomendação de túnel SSH ou reverse proxy com TLS (README-SERVER documenta os dois).
5. Nome do fork: ainda pendente (ver seção 7 do PLAN). O binário se chama `iloader-server` provisoriamente.

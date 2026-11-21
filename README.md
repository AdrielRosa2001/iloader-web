# iloader-web

Versão headless do [iloader](https://github.com/nab138/iloader) (de [nab138](https://github.com/nab138)): roda num PC ou servidor Linux, sem interface gráfica, e é usada pelo navegador de qualquer máquina da rede. Serve para instalar o SideStore (ou outros IPAs) e gerenciar pairing files de um iPhone ligado por USB nesse computador.

> **Projeto não oficial**, sem afiliação nem endosso do iloader. Site oficial do iloader: <https://iloader.app>. Para o app desktop, use o [repositório original](https://github.com/nab138/iloader).

```
navegador (notebook/celular) ──▶ iloader-web (PC Linux, Docker) ──usbmuxd──▶ iPhone (USB)
```

## Funcionalidades

As mesmas do app desktop, pela interface web:

- Instalar SideStore (ou LiveContainer + SideStore), importar o certificado e colocar os pairing files automaticamente
- Instalar qualquer IPA (enviado pelo navegador)
- Gerenciar pairing files em apps como StikDebug, SideStore, Protokolle etc.
- Ver e revogar certificados de desenvolvimento e App IDs
- Login no Apple ID com 2FA pela interface
- Acesso protegido por token

## Passo a passo: subir o container num PC Linux

Testado no Ubuntu/Debian. Em outras distros, troque o `apt` pelo gerenciador de pacotes equivalente.

### 1. Instalar Docker e usbmuxd

```sh
sudo apt update
sudo apt install -y git usbmuxd libimobiledevice-utils
curl -fsSL https://get.docker.com | sudo sh
sudo usermod -aG docker $USER
```

Saia da sessão e entre de novo (ou rode `newgrp docker`) para usar o `docker` sem `sudo`. Confira com `docker compose version`.

O `usbmuxd` fica no host: é ele que conversa com o iPhone pela USB. O container não recebe acesso direto à USB.

### 2. Ligar e parear o iPhone

1. Ligue o iPhone no PC pelo cabo USB.
2. Desbloqueie o iPhone e toque em **Confiar** quando ele perguntar.
3. Confira se o PC enxerga o aparelho:

   ```sh
   idevice_id -l
   ```

   Deve aparecer o UDID do iPhone. Se não aparecer, desconecte e conecte de novo, e confira se o `usbmuxd` está rodando (`systemctl status usbmuxd`).

### 3. Baixar o projeto

```sh
git clone -b web-headless https://github.com/AdrielRosa2001/iloader-web.git
cd iloader-web
```

### 4. (Opcional) Configurar

Por padrão a interface fica só em `127.0.0.1:8095`, ou seja, acessível só pelo próprio PC. Para mudar, crie um arquivo `.env` na pasta do projeto:

```sh
# .env
ILOADER_PORT=8095            # porta no PC
ILOADER_HOST_BIND=127.0.0.1  # 0.0.0.0 ou o IP da LAN/Tailscale para acessar de outras máquinas
```

Veja a seção [Acesso por outras máquinas](#acesso-por-outras-máquinas) antes de expor na rede.

### 5. Subir o container

```sh
docker compose up -d --build
```

O primeiro build compila tudo dentro da imagem (Rust + interface) e leva alguns minutos. Não precisa instalar Rust nem Node no PC.

### 6. Pegar o token de acesso

```sh
docker compose exec iloader cat /data/access_token
```

### 7. Usar

1. Abra <http://localhost:8095> no navegador e cole o token.
2. Selecione o iPhone na lista de dispositivos.
3. Entre com o Apple ID (o código 2FA é digitado na própria interface).
4. Escolha a ação, por exemplo instalar o SideStore.

### Comandos úteis

| Ação | Comando |
| --- | --- |
| Ver logs | `docker compose logs -f iloader` |
| Parar | `docker compose down` |
| Reiniciar | `docker compose restart iloader` |
| Atualizar | `git pull && docker compose up -d --build` |

Os dados (anisette, pairing, contas, token) ficam no volume `iloader-data` e são preservados ao atualizar ou recriar o container. O login no Apple ID fica só em memória: depois de reiniciar o container, é preciso entrar de novo.

## Acesso por outras máquinas

O serviço manipula a **senha do Apple ID** e o **pairing file** do iPhone. Sem HTTPS, o token e a senha trafegam em texto pela rede. Prefira uma destas opções:

- **Túnel SSH** (mais simples, mantém o padrão `127.0.0.1`). No notebook:

  ```sh
  ssh -N -L 8095:127.0.0.1:8095 usuario@ip-do-pc
  ```

  e abra <http://localhost:8095>.
- **Tailscale/VPN**: defina `ILOADER_HOST_BIND` com o IP da VPN no `.env`. O tráfego já vai cifrado pela VPN.
- **Reverse proxy com HTTPS** (Caddy, nginx): defina `ILOADER_COOKIE_SECURE=1` no `.env`.

Detalhes, exemplos de proxy, instalação sem Docker (systemd), variáveis de configuração e a API estão em [README-SERVER.md](README-SERVER.md).

## Problemas comuns

- **`503 Service Temporarily Unavailable` em `gsa.apple.com`**: a Apple recusa a identificação de cliente usada no login. Atualize (`git pull && docker compose up -d --build`); se persistir, troque o servidor anisette nas configurações.
- **Nenhum dispositivo aparece**: confira `idevice_id -l` no PC, desbloqueie o iPhone e toque em **Confiar**.
- **Token esquecido**: `docker compose exec iloader cat /data/access_token`.
- Para outros erros, veja os logs (`docker compose logs -f iloader` ou "View Logs" na interface, com nível "Debug").

## Desenvolvimento

```sh
cargo run -p iloader-server   # API em :8080
bun run dev                   # interface em :1420, com proxy de /api para :8080
cargo test --workspace        # testes sem hardware
```

- `crates/iloader-core`: lógica do iloader (login, pairing, instalação), sem Tauri.
- `crates/iloader-server`: servidor HTTP/WebSocket que serve a interface e a API.
- `src/`: a interface React do app original, com os módulos do Tauri trocados por `src/platform/`.
- `src-tauri/`: app desktop original, mantido para facilitar merges do upstream.

## Créditos

Todo o trabalho de sideload é do [iloader](https://github.com/nab138/iloader) e das bibliotecas que ele usa:

- [iloader](https://github.com/nab138/iloader) por [nab138](https://github.com/nab138), com ícone de [Transistor](https://github.com/transistor-exe), UI melhorada por [StephenDev0](https://github.com/StephenDev0) e traduções da comunidade
- [isideload](https://github.com/nab138/isideload) para instalar apps
- [idevice](https://github.com/jkcoxson/idevice) por [jkcoxson](https://github.com/jkcoxson) para comunicar com dispositivos iOS
- [apple-codesign-quick](https://github.com/Dadoum/apple-codesign-quick) e [Sideloader](https://github.com/Dadoum/Sideloader) por [Dadoum](https://github.com/Dadoum)
- [Impactor](https://github.com/claration/Impactor) por [claration](https://github.com/claration)
- [idevice_pair](https://github.com/jkcoxson/idevice_pair)

## Licença

Código-fonte original Copyright (C) 2026 nab138, sob a licença MIT. Veja [LICENSE](LICENSE).

O nome "iloader", os logos e demais materiais de marca não estão sob a licença MIT e seguem o [LICENSE-BRANDING](LICENSE-BRANDING). Este fork não é oficial nem endossado pelo projeto iloader; site oficial: <https://iloader.app>, código original: <https://github.com/nab138/iloader>.

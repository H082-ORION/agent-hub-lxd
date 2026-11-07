# agent-hub-lxd

[![CI](https://github.com/h082-orion/agent-hub-lxd/actions/workflows/ci.yml/badge.svg)](https://github.com/h082-orion/agent-hub-lxd/actions/workflows/ci.yml)

Agent d'hôte LXD pour [opencode-hub](https://github.com/h082-orion/opencode-hub) : il crée les **instances Cloud**
du Hub sous forme de VM LXD.

> [!WARNING]
> **Projet expérimental.** L'agent n'a pas encore été éprouvé sur un vrai serveur LXD ni en production. Le
> protocole, la configuration et le comportement peuvent changer sans préavis.

> [!NOTE]
> Ce projet est indépendant. Il n'est **ni lié, ni affilié, ni soutenu** par l'équipe officielle
> d'[OpenCode](https://github.com/anomalyco/opencode).

```
Hub ◄── WebSocket sortant ── agent-hub-lxd (serveur Ubuntu, même derrière NAT)
 ▲                               │ API LXD (socket unix local)
 │                               ▼
 └── tunnel machine ── VM hub-<id>, clonée du modèle, opencode agent déjà installé
```

1. Un utilisateur demande une instance Cloud sur le Hub.
2. Le Hub crée un code d'appairage à usage unique et envoie `create` à l'agent.
3. L'agent vérifie la place restante, clone la VM modèle, la démarre et attend son agent LXD.
4. Il dépose le code dans la VM et lance `hub-pair`, qui exécute `opencode agent join`.
5. La VM apparaît dans le Hub comme une machine ordinaire.

L'agent remonte en continu la capacité de l'hôte et les métriques des instances. Protocole complet :
[`docs/protocol.md`](docs/protocol.md).

## Installation sur l'hôte

Prérequis : Ubuntu avec LXD (snap) et la virtualisation matérielle (`lxc info | grep -i virtual`).

```bash
cargo build --release
sudo install -m 0755 target/release/agent-hub-lxd /usr/local/bin/
sudo useradd --system --no-create-home --shell /usr/sbin/nologin agent-hub-lxd
sudo install -d -m 0750 -g agent-hub-lxd /etc/agent-hub-lxd
sudo install -m 0640 -g agent-hub-lxd packaging/config.example.toml /etc/agent-hub-lxd/config.toml
sudo install -m 0644 packaging/agent-hub-lxd.service /etc/systemd/system/
sudo install -d -m 0700 -o agent-hub-lxd -g agent-hub-lxd /var/lib/agent-hub-lxd
```

### Projet LXD dédié

L'agent ne voit que son projet. Il n'agit que sur les VM nommées `hub-<id>` et marquées
`user.agent-hub-lxd.instance=<id>`.

```bash
lxc project create hub -c features.images=false -c features.profiles=true \
  -c restricted=true -c restricted.virtual-machines.lowlevel=block \
  -c limits.virtual-machines=20
lxc profile device add default root disk path=/ pool=default --project hub
lxc profile device add default eth0 nic network=hubbr0 name=eth0 --project hub   # réseau dédié, voir Sécurité
```

### VM modèle

```bash
lxc launch ubuntu:24.04 opencode-template --vm --project hub
lxc exec opencode-template --project hub -- bash
#   useradd -m -s /bin/bash opencode
#   installer opencode dans /usr/local/bin/opencode
#   copier template/hub-pair dans /usr/local/sbin/hub-pair (0755)
#   copier template/opencode-agent.service dans /etc/systemd/system/ (sans l'activer)
#   nettoyer avant de figer le modèle :
#     cloud-init clean --logs --machine-id ; truncate -s0 /etc/machine-id
#     rm -f /etc/ssh/ssh_host_*   (régénérées au démarrage)
#     vérifier qu'aucun ~opencode/.local/share/opencode/agent.json n'existe
lxc stop opencode-template --project hub
```

Le modèle ne doit contenir **aucun** enrôlement, jeton ou identité machine : tous les clones les partageraient.
`agent-hub-lxd status` vérifie que le modèle est bien une VM arrêtée.

### Enrôlement et démarrage

```bash
# Code « hôte » créé depuis le tableau de bord du Hub, demandé sans écho :
sudo -u agent-hub-lxd agent-hub-lxd join https://hub.exemple.fr
sudo -u agent-hub-lxd agent-hub-lxd status        # LXD, modèle, capacité
sudo systemctl enable --now agent-hub-lxd
journalctl -u agent-hub-lxd -f
```

`agent-hub-lxd leave` oublie l'enrôlement. Révoque aussi l'hôte sur le Hub.

## Sécurité

- **Aucun port ouvert** : l'agent ouvre une connexion WebSocket sortante (`wss://`). Le HTTP en clair n'est accepté
  que vers le loopback, ou avec `--insecure` explicite.
- **Jeton de l'hôte** : il est stocké dans `/var/lib/agent-hub-lxd/agent.json` en `0600`, dans un dossier `0700`,
  et écrit de façon atomique. L'agent refuse de démarrer si le fichier est lisible par d'autres. Le jeton n'est
  jamais journalisé.
- **Codes** : le code d'enrôlement de l'hôte est lu sans écho ou sur stdin, jamais en argument. Le code d'appairage
  d'une VM n'est jamais journalisé, ni passé en argument, ni mis dans la configuration LXD (lisible par
  `lxc config show`). Il est déposé dans `/run` (tmpfs) en `0600`, et `hub-pair` le supprime.
- **L'agent n'accorde qu'une confiance limitée au Hub** :
  - Les identifiants sont validés strictement avant d'entrer dans une URL LXD.
  - Les limites sont plafonnées par la configuration locale et la place restante, quelles que soient celles demandées.
  - Seules les VM marquées et nommées par l'agent sont touchées. Le modèle et le reste de LXD sont hors d'atteinte.
  - L'URL transmise aux VM est celle de l'enrôlement, jamais une URL envoyée par le Hub.
- **Pas de surallocation** : la capacité compte les limites allouées. Une vérification et une création ne peuvent
  pas s'entrelacer, et une seule opération à la fois est permise par instance.
- **Échec propre** : une VM qui ne démarre pas ou ne s'appaire pas est supprimée.
- **Service durci** (`packaging/agent-hub-lxd.service`) : utilisateur dédié, aucune capability, système de fichiers
  en lecture seule, filtrage des appels système. Le code 78 (jeton révoqué, protocole ou configuration invalides)
  arrête le service sans boucle de redémarrage.
- **À faire côté LXD** :
  - Mettre le projet en `restricted=true`.
  - Donner aux VM un réseau dédié dont les ACL bloquent l'hôte et le LAN : `lxc network acl` avec des règles
    `egress` `reject` vers `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` et l'IP de l'hôte, puis Internet
    autorisé.
  - Le groupe `lxd` équivaut à root sur l'hôte : réserver la machine à cet usage.

## Développement

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

La CI GitHub Actions (`.github/workflows/ci.yml`) lance ces vérifications et un build release à chaque push sur
`main` et sur chaque pull request. Le binaire Linux x86_64 est disponible en artefact du workflow.

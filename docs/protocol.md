# Protocole Hub ↔ agent LXD (version 1)

Contrat entre `agent-hub-lxd` et le Hub opencode-hub. L'agent LXD est un **hôte** : il ne sert pas de sessions,
il crée des VM. Chaque VM devient ensuite une **machine** ordinaire du Hub grâce à `opencode agent join`, avec le
protocole machine existant (`specs/remote-machines.md` d'opencode-hub). Ce document ne couvre que l'hôte.

## Enrôlement

`POST /hub/host/enroll`, JSON `{ code, name }`, puis réponse `200 { host: { id, name }, token, url }`.

- `code` est un code à usage unique créé par un utilisateur depuis le tableau de bord. Il est distinct des codes
  machine, ou typé, pour qu'un code machine ne puisse pas enrôler un hôte.
- `token` sert de jeton de l'hôte : 32 octets aléatoires, stocké haché côté Hub, révocable.
- `url` est l'URL publique du Hub. L'agent s'y connecte et la transmet aux VM pour leur appairage.
- En cas d'erreur, le Hub répond `4xx { error }`. Il ne fait pas de redirection : l'agent ne les suit pas.
- L'agent refuse d'envoyer le code ou le jeton en HTTP clair, sauf vers le loopback, ou avec `--insecure` choisi
  explicitement.

## Connexion

`GET /hub/host/connect` est une montée en WebSocket avec `Authorization: Bearer <token>`. Un jeton inconnu ou
révoqué reçoit `401` à la montée, et l'agent s'arrête alors sans redémarrer.

Chaque message est un **texte JSON** avec un champ `type`, d'au plus 1 Mio. Les champs et types inconnus sont
ignorés des deux côtés.

Codes de fermeture envoyés par le Hub :

| Code | Sens                                      | Réaction de l'agent           |
| ---- | ----------------------------------------- | ----------------------------- |
| 4400 | version de protocole non prise en charge  | arrêt définitif (exit 78)     |
| 4401 | hôte révoqué pendant la connexion         | arrêt définitif (exit 78)     |
| 4409 | remplacé par une autre connexion du même hôte | reconnexion avec backoff  |
| autre | —                                        | reconnexion avec backoff      |

Backoff : de 1 s, doublé jusqu'à 30 s, ±20 %. Il revient à 1 s après 60 s de connexion stable.

## Messages

### Agent → Hub

| type      | quand                         | contenu                                                                                     |
| --------- | ----------------------------- | ------------------------------------------------------------------------------------------- |
| `hello`   | premier message               | `protocol: 1, version, name, platform: { os, arch, lxd_version, project }, capacity, instances: Instance[]` |
| `pong`    | en réponse à `ping`           | —                                                                                           |
| `result`  | fin d'une opération           | `id, ok, instance?: Instance, error?: { code, message }`                                    |
| `metrics` | toutes les 15 s (configurable) | `host: HostMetrics, capacity, instances: InstanceMetrics[]`                                |

### Hub → Agent

| type     | contenu                                                                                 |
| -------- | --------------------------------------------------------------------------------------- |
| `ready`  | `host: { id, name }`, à envoyer après `hello`. L'agent ignore toute opération reçue avant. |
| `ping`   | toutes les 25 s. Après 60 s sans `ping`, l'agent considère la connexion morte.           |
| `create` | `id, instance, limits?: { cpu?, memory_mib?, disk_gib? }, pairing: { code }`            |
| `start`  | `id, instance`                                                                          |
| `stop`   | `id, instance`                                                                          |
| `delete` | `id, instance`                                                                          |

- `id` est l'identifiant de requête renvoyé tel quel dans `result` : ASCII imprimable, 64 caractères au plus.
- `instance` est l'identifiant de l'instance Cloud, choisi par le Hub : `[a-z0-9-]`, 1 à 40 caractères, sans tiret
  au début ni à la fin. La VM s'appelle `hub-<instance>`.
- `limits` : les champs absents prennent les valeurs par défaut de l'hôte. Au-delà du maximum par instance, ou
  au-delà de la place restante, la réponse est l'erreur `capacity`.
- `pairing.code` est un code d'enrôlement **machine** à usage unique, créé par le Hub pour l'utilisateur qui demande
  l'instance (`[A-Za-z0-9-]`, 64 au plus). Le Hub retrouve l'instance quand la VM s'enrôle avec ce code, ce qui lui
  permet de rattacher la machine à l'instance Cloud. Prévoir une durée de vie couvrant le démarrage de la VM, soit
  plusieurs minutes.
- Les opérations s'exécutent en parallèle, mais une seule à la fois par instance (sinon erreur `busy`).
  `create` ne répond qu'une fois la VM démarrée et appairée, ce qui peut prendre quelques minutes.
- Si `create` échoue après la création de la VM (démarrage ou appairage), l'agent supprime la VM.
- Un `result` perdu par une déconnexion n'est pas renvoyé. Le Hub réconcilie son état avec `instances` du `hello`
  suivant.

Codes d'erreur : `invalid`, `busy`, `exists`, `not_found`, `capacity`, `lxd`, `pairing`, `timeout`.

### Types

```ts
Instance = { instance: string, status: "running" | "stopped" | ..., limits: Limits, created_at: string | null }
Limits = { cpu: number, memory_mib: number, disk_gib: number }
Resource = { total, reserved, allocated, available }   // entiers
Capacity = {
  cpu: Resource, memory_mib: Resource, disk_gib: Resource,
  instances: { count, max }, per_instance_max: Limits
}
HostMetrics = { cpu_threads, load: [1m, 5m, 15m], memory_total_bytes, memory_used_bytes, disk_total_bytes, disk_used_bytes }
InstanceMetrics = {
  instance, status, cpu_usage_ns, memory_usage_bytes, memory_total_bytes,
  disk_usage_bytes, network_rx_bytes, network_tx_bytes
}
```

`capacity` compte les **limites** allouées, pas la consommation réelle :
`available = total - reserved - allocated`. Le Hub peut ainsi refuser une demande avant de l'envoyer.
`cpu_usage_ns` et les compteurs réseau sont cumulatifs : le Hub calcule les débits par différence entre deux
`metrics`.

## Exemple

```json
→ {"type":"hello","protocol":1,"version":"0.1.0","name":"lxd-paris","platform":{...},"capacity":{...},"instances":[]}
← {"type":"ready","host":{"id":"h1","name":"lxd-paris"}}
← {"type":"create","id":"r1","instance":"k3x9","limits":{"cpu":2},"pairing":{"code":"ABCD-EFGH"}}
→ {"type":"result","id":"r1","ok":true,"instance":{"instance":"k3x9","status":"running","limits":{"cpu":2,"memory_mib":4096,"disk_gib":20},"created_at":"..."}}
```

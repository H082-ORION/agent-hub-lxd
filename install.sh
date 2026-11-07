#!/usr/bin/env bash
# install.sh — installation et mise en place d'agent-hub-lxd sur un serveur Ubuntu (LXD, systemd).
# install.sh — install and set up agent-hub-lxd on an Ubuntu server (LXD, systemd).
#
# Propose trois composants, à cocher dans un menu ou à passer en options :
#   - LXD     : installé par snap et initialisé s'il ne l'est pas déjà, avec un projet dédié et un réseau isolé ;
#   - Modèle  : la VM que l'agent clone pour chaque instance Cloud, avec opencode-hub installé et non enrôlé ;
#   - Agent   : agent-hub-lxd, relié au hub, en service systemd à démarrage automatique.
#
#   sudo ./install.sh                                    # menu interactif / interactive menu
#   sudo ./install.sh --lxd --template --opencode-binary /usr/local/bin/opencode
#   sudo ./install.sh --agent --hub-url https://hub.example.com --code <code> --service
#   ./install.sh --status                                # état / status
#   sudo ./install.sh --uninstall                        # désinstallation / uninstall
#
# Même interface que l'installateur d'opencode-hub (mêmes menus, même bannière, mêmes réglages) :
# langue au lancement (français par défaut) ou --lang fr|en / OPENCODE_HUB_LANG ; NO_COLOR=1, OPENCODE_HUB_ASCII=1,
# OPENCODE_HUB_VITESSE=0, OPENCODE_HUB_SANS_BANNIERE=1. Le script peut être relancé sans risque.

set -euo pipefail

ARGS_ORIGINAUX=("$@")
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Les commandes du snap LXD.
export PATH="$PATH:/snap/bin"

# ------------------------------------------------------------------ Bloc commun avec install.sh d'opencode-hub
# Langue, affichage, bannière, menus et saisies : repris tel quel, à garder identique.
# ================================================================== Langue

# Lue avant tout le reste : les messages d'erreur des options en dépendent.
LANGUE="${OPENCODE_HUB_LANG:-}"
_args=("$@")
for ((_i = 0; _i < ${#_args[@]}; _i++)); do
  case "${_args[_i]}" in
    --lang=*) LANGUE="${_args[_i]#--lang=}" ;;
    --lang) LANGUE="${_args[_i + 1]:-}" ;;
  esac
done
LANGUE_CHOISIE=0
[[ -n "$LANGUE" ]] && LANGUE_CHOISIE=1
case "${LANGUE,,}" in
  en*) LANGUE=en ;;
  fr* | '') LANGUE=fr ;;
  *) echo "✗ --lang : fr ou en / fr or en (reçu / got : $LANGUE)" >&2; exit 1 ;;
esac

# t "texte français" "english text" — le texte dans la langue choisie.
# DP : les deux-points, précédés d'une espace en français seulement (à recalculer si la langue change).
t() { if [[ "$LANGUE" == en ]]; then printf '%s' "$2"; else printf '%s' "$1"; fi; }
DP="$(t ' : ' ': ')"

# ================================================================== Affichage

if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
  bold=$'\e[1m'; dim=$'\e[2m'; red=$'\e[31m'; green=$'\e[32m'; yellow=$'\e[33m'; cyan=$'\e[36m'
  gris=$'\e[38;5;245m'; reset=$'\e[0m'
else
  bold=''; dim=''; red=''; green=''; yellow=''; cyan=''; gris=''; reset=''
fi
TTY=0; [[ -t 1 ]] && TTY=1
UTF8=0
if [[ "${OPENCODE_HUB_ASCII:-0}" != 1 && "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" == *[Uu][Tt][Ff]*8* ]]; then UTF8=1; fi

if ((UTF8)); then
  S_INFO='▸'; S_OK='✓'; S_WARN='!'; S_ERR='✗'; S_ASK='?'; H='─'; V='│'
  C_HG='╭'; C_HD='╮'; C_BG='╰'; C_BD='╯'; S_PTR='❯'; S_ON='●'; S_OFF='○'; S_COCHE='◉'; S_VIDE='○'; S_ELLIPSE='…'; S_SEP='·'
  SPIN=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')
else
  S_INFO='>'; S_OK='OK'; S_WARN='!'; S_ERR='x'; S_ASK='?'; H='-'; V='|'
  C_HG='+'; C_HD='+'; C_BG='+'; C_BD='+'; S_PTR='>'; S_ON='*'; S_OFF='o'; S_COCHE='[x]'; S_VIDE='[ ]'; S_ELLIPSE='...'; S_SEP='-'
  SPIN=('|' '/' '-' '\')
fi
LARGEUR="${COLUMNS:-$(tput cols 2>/dev/null || echo 80)}"
((LARGEUR > 100)) && LARGEUR=100
((LARGEUR < 50)) && LARGEUR=50
VITESSE="${OPENCODE_HUB_VITESSE:-1}"
OUI=0
MENU_AIDE=""

pause() { [[ "$VITESSE" == 0 ]] || sleep "$1"; }
cacher() { if ((TTY)); then printf '\e[?25l'; fi; }
montrer() { if ((TTY)); then printf '\e[?25h'; fi; }
trap montrer EXIT
trap 'montrer; echo; exit 130' INT

# Saisie possible : entrée standard sur un terminal, et pas de --yes.
interactif() { [[ -t 0 && "$OUI" != 1 ]]; }

info() { printf '%s%s%s %s\n' "$gris" "$S_INFO" "$reset" "$*"; }
ok()   { printf '%s%s%s %s\n' "$green" "$S_OK" "$reset" "$*"; }
warn() { printf '%s%s%s %s\n' "$yellow" "$S_WARN" "$reset" "$*" >&2; }
err()  { printf '%s%s %s%s\n' "$red" "$S_ERR" "$*" "$reset" >&2; }
die()  { err "$*"; exit 1; }
etape() { printf '\n%s%s== %s%s\n' "$gris" "$bold" "$*" "$reset"; }

# Longueur visible : sans séquences de couleur ni liens.
vlen() {
  local s
  s="$(sed -E 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\x1b\]8;;[^\x1b]*\x1b\\//g' <<<"$1")"
  printf '%d' "${#s}"
}
pad() { local n; n="$(vlen "$1")"; printf '%s%*s' "$1" $((n < $2 ? $2 - n : 0)) ''; }
rep() { local i out=''; for ((i = 0; i < $2; i++)); do out+="$1"; done; printf '%s' "$out"; }
tronque() {
  local n="$2"; ((n < 1)) && n=1
  if ((${#1} > n)); then printf '%s%s' "${1:0:n-1}" "$S_ELLIPSE"; else printf '%s' "$1"; fi
}
trait() { printf '%s%s%s\n' "$dim" "$(rep "$H" "$LARGEUR")" "$reset"; }
lien() { if ((TTY)); then printf '\e]8;;%s\e\\%s%s%s\e]8;;\e\\' "$1" "$cyan" "${2:-$1}" "$reset"; else printf '%s' "${2:-$1}"; fi; }
titre_onglet() { if ((TTY)); then printf '\e]0;%s\a' "$1"; fi; }

# cadre COULEUR TITRE LIGNE… — encart à bordure arrondie.
cadre() {
  local c="$1" titre="$2" l n w
  shift 2
  w=$(($(vlen "$titre") + 6))
  for l in "$@"; do n="$(vlen "$l")"; if ((n + 4 > w)); then w=$((n + 4)); fi; done
  if ((w > LARGEUR)); then w=$LARGEUR; fi
  printf '%s%s%s %s%s%s %s%s%s\n' "$c" "$C_HG" "$H" "$bold" "$titre" "$reset$c" "$(rep "$H" $((w - $(vlen "$titre") - 5)))" "$C_HD" "$reset"
  for l in "$@"; do
    printf '%s%s%s %s %s%s%s\n' "$c" "$V" "$reset" "$(pad "$l" $((w - 4)))" "$c" "$V" "$reset"
  done
  printf '%s%s%s%s%s\n' "$c" "$C_BG" "$(rep "$H" $((w - 2)))" "$C_BD" "$reset"
}

# banniere [TITRE] [SOUS-TITRE] — dégradé du noir (en haut) au blanc (en bas), en passant par le gris.
banniere() {
  [[ "${OPENCODE_HUB_SANS_BANNIERE:-0}" != 1 ]] || return 0
  local titre="${1:-agent-hub-lxd}" sous="${2:-installation}"
  local -a art=(
    ' ██████╗ ██████╗ ███████╗███╗   ██╗ ██████╗ ██████╗ ██████╗ ███████╗'
    '██╔═══██╗██╔══██╗██╔════╝████╗  ██║██╔════╝██╔═══██╗██╔══██╗██╔════╝'
    '██║   ██║██████╔╝█████╗  ██╔██╗ ██║██║     ██║   ██║██║  ██║█████╗'
    '██║   ██║██╔═══╝ ██╔══╝  ██║╚██╗██║██║     ██║   ██║██║  ██║██╔══╝'
    '╚██████╔╝██║     ███████╗██║ ╚████║╚██████╗╚██████╔╝██████╔╝███████╗'
    ' ╚═════╝ ╚═╝     ╚══════╝╚═╝  ╚═══╝ ╚═════╝ ╚═════╝ ╚═════╝ ╚══════╝'
    '                     ██╗  ██╗██╗   ██╗██████╗'
    '                     ██║  ██║██║   ██║██╔══██╗'
    '                     ███████║██║   ██║██████╔╝'
    '                     ██╔══██║██║   ██║██╔══██╗'
    '                     ██║  ██║╚██████╔╝██████╔╝'
    '                     ╚═╝  ╚═╝ ╚═════╝ ╚═════╝'
  )
  # Palette 256 couleurs, rampe des gris : 233 (presque noir) → 255 (blanc).
  local -a teinte=(233 235 237 239 241 243 245 247 249 251 253 255)
  local i
  echo
  if ((UTF8 && LARGEUR >= 72)); then
    for i in "${!art[@]}"; do
      if [[ -n "$reset" ]]; then printf '  \e[38;5;%sm%s%s\n' "${teinte[$i]}" "${art[$i]}" "$reset"; else printf '  %s\n' "${art[$i]}"; fi
      pause 0.03
    done
  else
    printf '  %s%sO P E N C O D E   H U B%s\n' "$gris" "$bold" "$reset"
  fi
  printf '\n  %s%s%s  %s%s %s%s\n\n' "$bold" "$titre" "$reset" "$dim" "$S_SEP" "$sous" "$reset"
  trait
}

# suivre "titre" commande [args…] — lance la commande en silence avec une attente animée et sa dernière ligne ;
# au bout, ✓ et la durée, ou ✗ et les dernières lignes du journal. Renvoie le code de la commande.
suivre() {
  local titre="$1"; shift
  local jr pid rc=0 debut=$SECONDS i=0 der l
  jr="$(mktemp /tmp/agent-hub-lxd-install.XXXXXX)"
  # Toujours en arrière-plan : la commande garde `set -e` (il serait ignoré dans un « cmd || rc=$? »).
  ("$@") >"$jr" 2>&1 &
  pid=$!
  if ((TTY)); then
    cacher
    while kill -0 "$pid" 2>/dev/null; do
      der="$(tail -c 2000 "$jr" 2>/dev/null | tr '\r' '\n' | sed -E 's/\x1b\[[0-9;?]*[A-Za-z]//g; /^[[:space:]]*$/d' | tail -n1)" || true
      printf '\r\e[K%s%s%s %s %s%s%s' "$gris" "${SPIN[i % ${#SPIN[@]}]}" "$reset" "$titre" "$dim" \
        "$(tronque "$der" $((LARGEUR - $(vlen "$titre") - 6)))" "$reset"
      sleep 0.1; i=$((i + 1))
    done
  else
    info "$titre…"
  fi
  wait "$pid" || rc=$?
  if ((TTY)); then printf '\r\e[K'; montrer; fi
  if ((rc == 0)); then
    ok "$titre ${dim}($((SECONDS - debut)) s)${reset}"
    rm -f "$jr"
  else
    err "$titre${DP}$(t "échec (code $rc)." "failed (code $rc).")"
    local -a ex=()
    while IFS= read -r l; do ex+=("$dim$(tronque "$l" $((LARGEUR - 6)))$reset"); done \
      < <(tr '\r' '\n' <"$jr" | sed -E 's/\x1b\[[0-9;?]*[A-Za-z]//g' | grep -v '^[[:space:]]*$' | tail -n 20)
    ((${#ex[@]})) || ex=("${dim}$(t "(aucune sortie)" "(no output)")${reset}")
    cadre "$red" "$(t "Dernières lignes" "Last lines")" "${ex[@]}" >&2
    echo "$(t "Journal complet" "Full log")${DP}$jr" >&2
  fi
  return "$rc"
}

# menu "titre" option… — choix (index à partir de 0) dans MENU_CHOIX ; renvoie 1 si abandon.
menu() {
  local titre="$1"; shift
  local -a opts=("$@"); local n=${#opts[@]} sel=0 i k r
  MENU_CHOIX=-1
  printf '\n%s%s%s\n' "$bold" "$titre" "$reset"
  if ((!TTY)) || ! interactif; then
    for i in "${!opts[@]}"; do printf '  %d. %s\n' "$((i + 1))" "${opts[$i]}"; done
    if ! interactif; then info "$(t "Non interactif : choix par défaut (1)." "Non-interactive: default choice (1).")"; MENU_CHOIX=0; return 0; fi
    printf '%s (1-%d)%s' "$(t "Votre choix" "Your choice")" "$n" "$DP"; read -r r || return 1
    [[ "$r" =~ ^[0-9]+$ ]] && ((r >= 1 && r <= n)) || return 1
    MENU_CHOIX=$((r - 1)); return 0
  fi
  printf '%s%s%s\n' "$dim" "${MENU_AIDE:-$(t "↑↓ ou chiffre · Entrée valide · q quitte" "↑↓ or number · Enter selects · q quits")}" "$reset"
  cacher
  while :; do
    for i in "${!opts[@]}"; do
      if ((i == sel)); then printf '\r\e[K  %s%s %s%s\n' "$gris$bold" "$S_PTR" "${opts[$i]}" "$reset"
      else printf '\r\e[K    %s\n' "${opts[$i]}"; fi
    done
    IFS= read -rsn1 k || { montrer; return 1; }
    if [[ "$k" == $'\e' ]]; then
      read -rsn2 -t 0.1 r || true
      case "$r" in '[A') ((sel = (sel + n - 1) % n)) || true ;; '[B') ((sel = (sel + 1) % n)) || true ;; esac
    elif [[ "$k" == '' ]]; then MENU_CHOIX=$sel; montrer; printf '\e[%dA\e[J' "$n"; ok "${opts[$sel]}"; return 0
    elif [[ "$k" == q ]]; then montrer; printf '\e[%dA\e[J' "$n"; return 1
    elif [[ "$k" =~ ^[1-9]$ ]] && ((k <= n)); then sel=$((k - 1)); fi
    printf '\e[%dA' "$n"
  done
}

# cocher "titre" option… — cases à cocher (flèches, espace, Entrée) ; indices cochés dans COCHES.
cocher() {
  local titre="$1"; shift
  local -a opts=("$@") etat=(); local n=${#opts[@]} sel=0 i k r case_
  COCHES=()
  for i in "${!opts[@]}"; do etat[i]=0; done
  printf '\n%s%s%s\n' "$bold" "$titre" "$reset"
  if ((!TTY)); then
    for i in "${!opts[@]}"; do printf '  %d. %s\n' "$((i + 1))" "${opts[$i]}"; done
    printf '%s : ' "$(t "Numéros séparés par des espaces" "Numbers separated by spaces")"; read -r r || return 1
    for k in $r; do [[ "$k" =~ ^[0-9]+$ ]] && ((k >= 1 && k <= n)) && COCHES+=("$((k - 1))"); done
    return 0
  fi
  printf '%s%s%s\n' "$dim" "$(t "↑↓ déplace · espace coche · Entrée valide · q quitte" "↑↓ moves · space toggles · Enter confirms · q quits")" "$reset"
  cacher
  while :; do
    for i in "${!opts[@]}"; do
      case_="$S_VIDE"; ((etat[i])) && case_="$green$S_COCHE$reset"
      if ((i == sel)); then printf '\r\e[K  %s%s%s %s %s\n' "$gris$bold" "$S_PTR" "$reset" "$case_" "${opts[$i]}"
      else printf '\r\e[K    %s %s\n' "$case_" "${opts[$i]}"; fi
    done
    IFS= read -rsn1 k || { montrer; return 1; }
    if [[ "$k" == $'\e' ]]; then
      read -rsn2 -t 0.1 r || true
      case "$r" in '[A') ((sel = (sel + n - 1) % n)) || true ;; '[B') ((sel = (sel + 1) % n)) || true ;; esac
    elif [[ "$k" == ' ' ]]; then etat[sel]=$((1 - etat[sel]))
    elif [[ "$k" == '' ]]; then
      for i in "${!opts[@]}"; do if ((etat[i])); then COCHES+=("$i"); fi; done
      montrer; printf '\e[%dA\e[J' "$n"; return 0
    elif [[ "$k" == q ]]; then montrer; printf '\e[%dA\e[J' "$n"; return 1; fi
    printf '\e[%dA' "$n"
  done
}

# demander VARIABLE "question" [défaut] [regex] [message d'erreur]
# La valeur déjà donnée en option sert de défaut ; hors interactif, elle est prise telle quelle.
demander() {
  local var="$1" question="$2" defaut="${!1:-${3:-}}" regex="${4:-}" msg="${5:-$(t "valeur invalide" "invalid value")}" r
  local invite="$gris$S_ASK$reset $question"
  [[ -z "$defaut" ]] || invite+=" ${dim}[$defaut]${reset}"
  if ! interactif; then
    [[ -n "$defaut" ]] || { err "$question${DP}$(t "valeur obligatoire en mode non interactif." "a value is required in non-interactive mode.")"; return 1; }
    [[ -z "$regex" || "$defaut" =~ $regex ]] || { err "$question${DP}$msg"; return 1; }
    printf -v "$var" '%s' "$defaut"; info "$question${DP}${bold}$defaut${reset}"; return 0
  fi
  while :; do
    read -r -p "$invite " r || return 1
    r="${r:-$defaut}"
    if [[ -z "$r" ]]; then err "$(t "valeur obligatoire." "a value is required.")"; continue; fi
    if [[ -n "$regex" && ! "$r" =~ $regex ]]; then err "$msg"; continue; fi
    printf -v "$var" '%s' "$r"; return 0
  done
}

# demander_secret VARIABLE "question" — saisie sans écho ; hors interactif, la valeur déjà donnée en option.
demander_secret() {
  local var="$1" question="$2" r
  if ! interactif; then
    [[ -n "${!var:-}" ]] || { err "$question${DP}$(t "valeur obligatoire en mode non interactif." "a value is required in non-interactive mode.")"; return 1; }
    return 0
  fi
  while :; do
    read -rs -p "$gris$S_ASK$reset $question ${dim}$(t "(non affiché)" "(hidden)")${reset} " r || return 1
    echo
    if [[ -n "$r" ]]; then printf -v "$var" '%s' "$r"; return 0; fi
    err "$(t "valeur obligatoire." "a value is required.")"
  done
}

# url_sure URL — HTTPS, ou HTTP vers cette machine seulement (même règle que « opencode agent join »).
url_sure() {
  local hote
  [[ "$1" == https://* ]] && return 0
  [[ "$1" == http://* ]] || return 1
  hote="${1#http://}"; hote="${hote%%/*}"
  if [[ "$hote" == \[* ]]; then hote="${hote%%]*}]"; else hote="${hote%%:*}"; fi
  hote="${hote,,}"
  [[ "$hote" == localhost || "$hote" == *.localhost || "$hote" == "[::1]" || "$hote" =~ ^127(\.[0-9]{1,3}){3}$ ]]
}

# oui_non "question" [o|n] — renvoie 0 pour oui ; hors interactif, le défaut.
oui_non() {
  local q="$1" def="${2:-o}" r sfx
  if [[ "$def" == n ]]; then sfx="$(t '[o/N]' '[y/N]')"; else sfx="$(t '[O/n]' '[Y/n]')"; fi
  if ! interactif; then [[ "$def" == o ]]; return; fi
  while :; do
    read -r -p "$gris$S_ASK$reset $q ${dim}$sfx${reset} " r || return 1
    r="${r:-$def}"
    case "${r,,}" in o|oui|y|yes) return 0 ;; n|non|no) return 1 ;; *) err "$(t "répondre par o ou n." "answer y or n.")" ;; esac
  done
}

# confirmer_grave "ce qui sera perdu" MOT — encart rouge ; il faut retaper MOT. Avec --yes : accepté.
confirmer_grave() {
  local quoi="$1" mot="$2" r
  cadre "$red" "$(t "Action irréversible" "Irreversible action")" "$quoi" "" "$(t "Pour confirmer, tapez" "To confirm, type")${DP}${bold}$mot${reset}"
  [[ "$OUI" == 1 ]] && return 0
  interactif || { err "$(t "confirmation impossible hors terminal (--yes pour passer outre)." "cannot confirm without a terminal (--yes to override).")"; return 1; }
  read -r -p "$red$S_ASK$reset " r || return 1
  [[ "$r" == "$mot" ]] || { err "$(t "mot différent : rien n'a été effacé." "different word: nothing was deleted.")"; return 1; }
}

aide_titre() { printf '\n%s%s%s\n' "$gris$bold" "$1" "$reset"; }
aide_opt() { printf '  %s%s%s %s%s%s\n' "$bold" "$(pad "$1" 24)" "$reset" "$dim" "$2" "$reset"; }

# ------------------------------------------------------------------ Fin du bloc commun

# ================================================================== Réglages

INSTALLER_LXD=0
INSTALLER_MODELE=0
INSTALLER_AGENT=0
SERVICE=""
ISOLER=""
STOCKAGE=""
HUB_URL=""
CODE=""
INSECURE=0
NOM_HOTE=""
PROJET="hub"
MODELE="opencode-template"
POOL=""
IMAGE="ubuntu:24.04"
OPENCODE_BINAIRE=""
BINARY=""
CONSTRUIRE=0
REFAIRE_MODELE=0
MODE=installer
PURGER=0

RESEAU="hubbr0"
ACL="hub-isolation"
UTILISATEUR="agent-hub-lxd"
BIN_CIBLE="/usr/local/bin/agent-hub-lxd"
CONF_DIR="/etc/agent-hub-lxd"
CONF="$CONF_DIR/config.toml"
ETAT_DIR="/var/lib/agent-hub-lxd"
UNIT="/etc/systemd/system/agent-hub-lxd.service"
# Fichier où la VM trouve son code d'appairage, et commande qui le lit : les mêmes que dans l'agent.
APPAIRAGE="/usr/local/sbin/hub-pair"
MARQUE="# Managed by agent-hub-lxd install.sh"

aide() {
  t "Installe LXD, la VM modèle des instances Cloud et l'agent agent-hub-lxd qui relie ce serveur à un hub." \
    "Installs LXD, the cloud instances' template VM and the agent-hub-lxd agent that connects this server to a hub."
  echo
  t "Sans option, un menu propose les composants et chaque réglage." \
    "Without options, a menu offers the components and each setting."
  echo
  aide_titre "$(t "Utilisation" "Usage")"
  echo "  sudo ./install.sh $(t "[composants] [options]" "[components] [options]")"
  aide_titre "$(t "Composants" "Components")"
  aide_opt "--lxd" "$(t "installer et initialiser LXD, projet dédié et réseau isolé" "install and initialize LXD, dedicated project and isolated network")"
  aide_opt "--template" "$(t "créer la VM modèle avec opencode-hub" "create the template VM with opencode-hub")"
  aide_opt "--agent" "$(t "installer agent-hub-lxd et le relier au hub" "install agent-hub-lxd and connect it to the hub")"
  aide_titre "LXD"
  aide_opt "--storage zfs|dir" "$(t "stockage à créer si LXD n'est pas initialisé (défaut : zfs)" "storage to create if LXD is not initialized (default: zfs)")"
  aide_opt "--project $(t "<nom>" "<name>")" "$(t "projet LXD des instances (défaut : hub)" "LXD project of the instances (default: hub)")"
  aide_opt "--storage-pool $(t "<nom>" "<name>")" "$(t "pool des disques (défaut : le premier pool)" "pool of the disks (default: the first pool)")"
  aide_opt "--isolate" "$(t "réseau dédié $RESEAU : les VM ne joignent ni l'hôte ni le réseau local" "dedicated $RESEAU network: VMs reach neither the host nor the local network")"
  aide_opt "--no-isolate" "$(t "les VM utilisent le réseau par défaut de LXD" "VMs use LXD's default network")"
  aide_titre "$(t "VM modèle" "Template VM")"
  aide_opt "--template-name $(t "<nom>" "<name>")" "$(t "nom de la VM modèle (défaut : opencode-template)" "template VM name (default: opencode-template)")"
  aide_opt "--image <image>" "$(t "image de départ (défaut : ubuntu:24.04)" "base image (default: ubuntu:24.04)")"
  aide_opt "--opencode-binary $(t "<chemin>" "<path>")" "$(t "binaire opencode-hub à installer dans la VM" "opencode-hub binary to install in the VM")"
  aide_opt "--rebuild-template" "$(t "recréer la VM modèle si elle existe" "recreate the template VM if it exists")"
  aide_titre "Agent"
  aide_opt "--hub-url <url>" "$(t "URL du hub à rejoindre" "URL of the hub to join")"
  aide_opt "--code <code>" "$(t "code de serveur Cloud à usage unique (sinon demandé sans affichage)" "one-time cloud server code (otherwise asked without echo)")"
  aide_opt "--name $(t "<nom>" "<name>")" "$(t "nom du serveur sur le hub (défaut : nom d'hôte)" "server name on the hub (default: hostname)")"
  aide_opt "--insecure" "$(t "accepter un hub en http:// sur une autre machine (code et jeton en clair)" "accept a plain http:// hub on another machine (code and token in clear text)")"
  aide_opt "--binary $(t "<chemin>" "<path>")" "$(t "binaire agent-hub-lxd à installer dans $BIN_CIBLE" "agent-hub-lxd binary to install to $BIN_CIBLE")"
  aide_opt "--build" "$(t "construire l'agent depuis ce dépôt (cargo)" "build the agent from this repository (cargo)")"
  aide_opt "--service" "$(t "installer le service systemd (démarrage automatique)" "install the systemd service (start at boot)")"
  aide_opt "--no-service" "$(t "ne pas installer de service systemd" "do not install the systemd service")"
  aide_titre "Maintenance"
  aide_opt "--status" "$(t "état de LXD, du modèle et de l'agent" "status of LXD, the template and the agent")"
  aide_opt "--uninstall" "$(t "désinstaller (les composants cochés, ou ceux passés en options)" "uninstall (the checked components, or those given as options)")"
  aide_opt "--purge" "$(t "avec --uninstall : enrôlement, configuration, binaire ; le snap LXD avec --lxd" "with --uninstall: enrollment, configuration, binary; the LXD snap with --lxd")"
  aide_titre "$(t "Général" "General")"
  aide_opt "--lang fr|en" "$(t "langue sans passer par le menu du lancement (ou OPENCODE_HUB_LANG)" "language without the startup menu (or OPENCODE_HUB_LANG)")"
  aide_opt "-y, --yes" "$(t "aucune question : options et valeurs par défaut" "no questions: options and default values")"
  aide_opt "-h, --help" "$(t "cette aide" "this help")"
  aide_titre "$(t "Exemples" "Examples")"
  echo "  sudo ./install.sh"
  echo "  sudo ./install.sh --lxd --template --agent --opencode-binary /usr/local/bin/opencode --hub-url https://hub.example.com --service"
  echo "  ./install.sh --status"
  echo "  sudo ./install.sh --uninstall --agent"
  echo
}

valeur() { [[ $# -ge 2 && -n "$2" && "$2" != --* ]] || die "$1 $(t "attend une valeur" "requires a value")"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --lxd) INSTALLER_LXD=1; shift ;;
    --template) INSTALLER_MODELE=1; shift ;;
    --agent) INSTALLER_AGENT=1; shift ;;
    --storage) valeur "$@"; STOCKAGE="$2"; shift 2 ;;
    --project) valeur "$@"; PROJET="$2"; shift 2 ;;
    --storage-pool) valeur "$@"; POOL="$2"; shift 2 ;;
    --isolate) ISOLER=1; shift ;;
    --no-isolate) ISOLER=0; shift ;;
    --template-name) valeur "$@"; MODELE="$2"; shift 2 ;;
    --image) valeur "$@"; IMAGE="$2"; shift 2 ;;
    --opencode-binary) valeur "$@"; OPENCODE_BINAIRE="$2"; shift 2 ;;
    --rebuild-template) REFAIRE_MODELE=1; shift ;;
    --hub-url) valeur "$@"; HUB_URL="$2"; shift 2 ;;
    --code) valeur "$@"; CODE="$2"; shift 2 ;;
    --name) valeur "$@"; NOM_HOTE="$2"; shift 2 ;;
    --insecure) INSECURE=1; shift ;;
    --binary) valeur "$@"; BINARY="$2"; shift 2 ;;
    --build) CONSTRUIRE=1; shift ;;
    --service) SERVICE=1; shift ;;
    --no-service) SERVICE=0; shift ;;
    --lang) valeur "$@"; shift 2 ;;
    --lang=*) shift ;;
    -y | --yes) OUI=1; shift ;;
    --status) MODE=etat; shift ;;
    --uninstall) MODE=desinstaller; shift ;;
    --purge) PURGER=1; shift ;;
    -h | --help) MODE=aide; shift ;;
    *) aide >&2; die "$(t "option inconnue" "unknown option")${DP}$1" ;;
  esac
done

# Bannière, puis choix de la langue par un menu dès le lancement (sauf si --lang ou OPENCODE_HUB_LANG l'ont fixée).
case "$MODE" in
  aide) sous_fr="agent LXD · aide de l'installateur"; sous_en="LXD agent · installer help" ;;
  etat) sous_fr="agent LXD · état"; sous_en="LXD agent · status" ;;
  desinstaller) sous_fr="agent LXD · désinstallation"; sous_en="LXD agent · uninstall" ;;
  *) sous_fr="agent LXD · installation"; sous_en="LXD agent · installation" ;;
esac
titre_onglet "agent-hub-lxd · $(t "$sous_fr" "$sous_en")"
if ((!LANGUE_CHOISIE)) && interactif && ((TTY)); then
  banniere "agent-hub-lxd" "$(t "$sous_fr" "$sous_en")"
  MENU_AIDE="↑↓ · Entrée / Enter · q"
  menu "Langue / Language" "Français" "English" || die "abandon / aborted."
  MENU_AIDE=""
  if ((MENU_CHOIX == 1)); then LANGUE=en; DP=': '; fi
  titre_onglet "agent-hub-lxd · $(t "$sous_fr" "$sous_en")"
else
  banniere "agent-hub-lxd" "$(t "$sous_fr" "$sous_en")"
fi

RE_URL='^https?://[^[:space:]/]+(/[^[:space:]]*)?$'
# Les noms que l'agent accepte dans sa configuration (config.rs, is_lxd_name).
RE_NOM_LXD='^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$'
RE_IMAGE='^[A-Za-z0-9._:/-]+$'

# ================================================================== Outils

dossier_de() { getent passwd "$1" | cut -d: -f6; }

# en_tant_que UTILISATEUR commande… — lance la commande sous cet utilisateur, avec son propre environnement.
en_tant_que() {
  local u="$1" h; shift
  h="$(dossier_de "$u")"
  local -a env_u=(env HOME="$h" USER="$u" LOGNAME="$u" OPENCODE_HUB_LANG="$LANGUE"
    PATH="$h/.cargo/bin:$h/.opencode/bin:$h/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/snap/bin")
  if [[ "$u" == root ]]; then "${env_u[@]}" "$@"; else runuser -u "$u" -- "${env_u[@]}" "$@"; fi
}

version_de() { local v; v="$("$1" --version 2>/dev/null | head -n1)" || true; printf '%s\n' "${v:-$(t "version inconnue" "unknown version")}"; }

apt_installer() {
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -q
  apt-get install -y -q "$@"
}

lxd_present() { command -v lxc >/dev/null && command -v lxd >/dev/null; }
# LXD initialisé : il a au moins un pool de stockage.
lxd_initialise() { lxd_present && [[ -n "$(lxc storage list -f csv 2>/dev/null | head -n1)" ]]; }
premier_pool() { lxc storage list -f csv 2>/dev/null | head -n1 | cut -d, -f1; }
projet_existe() { lxc project show "$1" >/dev/null 2>&1; }
# etat_vm NOM PROJET — RUNNING, STOPPED…, ou vide si elle n'existe pas.
etat_vm() { lxc list "^$1\$" --project "$2" -f csv -c s 2>/dev/null | head -n1; }
kvm_disponible() { [[ -e /dev/kvm ]]; }

# Valeur d'une clé `cle = "valeur"` de la configuration de l'agent.
conf_valeur() { sed -n "s/^[[:space:]]*$1[[:space:]]*=[[:space:]]*\"\\(.*\\)\"/\\1/p" "$CONF" 2>/dev/null | head -n1 || true; }
# URL du hub de l'enrôlement (agent.json), sans le jeton.
hub_enrole() { sed -n 's/^[[:space:]]*"url":[[:space:]]*"\([^"]*\)".*/\1/p' "$ETAT_DIR/agent.json" 2>/dev/null | head -n1 || true; }

# Ce que la configuration en place donne, à défaut d'options.
if [[ -f "$CONF" ]]; then
  [[ "$PROJET" != hub ]] || PROJET="$(conf_valeur project)"; PROJET="${PROJET:-hub}"
  [[ "$MODELE" != opencode-template ]] || MODELE="$(conf_valeur template)"; MODELE="${MODELE:-opencode-template}"
  [[ -n "$POOL" ]] || POOL="$(conf_valeur storage_pool)"
fi

# ================================================================== État et désinstallation

etat_unite() {
  systemctl list-unit-files "$1.service" --no-legend 2>/dev/null | grep -q . || { echo absent; return; }
  if systemctl is-active --quiet "$1"; then echo actif; else echo inactif; fi
}
auto_unite() { [[ "$(systemctl is-enabled "$1" 2>/dev/null || true)" == enabled ]]; }

pastille() {
  case "$1" in
    actif) printf '%s%s%s %s' "$green" "$S_ON" "$reset" "$(t "en marche" "running")" ;;
    inactif) printf '%s%s%s %s' "$yellow" "$S_ON" "$reset" "$(t "arrêté" "stopped")" ;;
    installe) printf '%s%s%s %s' "$green" "$S_ON" "$reset" "$(t "installé" "installed")" ;;
    *) printf '%s%s %s%s' "$dim" "$S_OFF" "$(t "absent" "missing")" "$reset" ;;
  esac
}

ligne_etat() {
  local nom="$1" statut="$2" auto="$3" detail="$4" demarrage=''
  case "$auto" in
    1) demarrage="${green}auto${reset}" ;;
    0) demarrage="${dim}$(t "manuel" "manual")${reset}" ;;
  esac
  printf '%s %s %s %s' "$(pad "${bold}$nom${reset}" 8)" "$(pad "$(pastille "$statut")" 12)" "$(pad "$demarrage" 7)" "$dim$detail$reset"
}

afficher_etat() {
  local -a l=()
  local e d hub

  # LXD
  if lxd_present; then
    d="LXD $(lxc version 2>/dev/null | sed -n 's/^Server version: //p' | head -n1)"
    if lxd_initialise; then d+=" $S_SEP pool $(premier_pool)"; else d+=" $S_SEP ${yellow}$(t "non initialisé" "not initialized")${reset}$dim"; fi
    if projet_existe "$PROJET"; then d+=" $S_SEP $(t "projet" "project") $PROJET"; else d+=" $S_SEP ${yellow}$(t "projet $PROJET absent" "no $PROJET project")${reset}$dim"; fi
    if kvm_disponible; then e=installe; else e=inactif; d+=" $S_SEP ${red}$(t "pas de KVM" "no KVM")${reset}$dim"; fi
    l+=("$(ligne_etat LXD "$e" - "$d")")
  else
    l+=("$(ligne_etat LXD absent - "")")
  fi

  # Modèle
  e="$(lxd_present && etat_vm "$MODELE" "$PROJET" || true)"
  case "$e" in
    STOPPED) l+=("$(ligne_etat "$(t "Modèle" "Template")" installe - "$MODELE $S_SEP $(t "arrêtée, prête à cloner" "stopped, ready to clone")")") ;;
    "") l+=("$(ligne_etat "$(t "Modèle" "Template")" absent - "$MODELE")") ;;
    *) l+=("$(ligne_etat "$(t "Modèle" "Template")" inactif - "$MODELE $S_SEP ${yellow}${e,,}, $(t "doit être arrêtée" "must be stopped")${reset}$dim")") ;;
  esac

  # Agent
  e="$(etat_unite agent-hub-lxd)"
  if [[ "$e" == absent ]]; then
    d=""
    [[ -x "$BIN_CIBLE" ]] && d="$BIN_CIBLE $S_SEP $(version_de "$BIN_CIBLE") $S_SEP $(t "sans service" "no service")"
    l+=("$(ligne_etat Agent "$( [[ -x "$BIN_CIBLE" ]] && echo inactif || echo absent)" - "$d")")
  else
    d="$(version_de "$BIN_CIBLE")"
    if [[ $EUID -eq 0 ]]; then
      hub="$(hub_enrole)"
      if [[ -n "$hub" ]]; then d+=" $S_SEP $hub"; else d+=" $S_SEP ${red}$(t "non enrôlé" "not enrolled")${reset}$dim"; fi
    fi
    l+=("$(ligne_etat Agent "$e" "$(auto_unite agent-hub-lxd && echo 1 || echo 0)" "$d")")
  fi

  cadre "$gris" "$(t "État" "Status")" "${l[@]}"
  [[ $EUID -eq 0 ]] || info "${dim}$(t "Lancer avec sudo pour voir aussi l'enrôlement de l'agent." "Run with sudo to also see the agent's enrollment.")${reset}"
  echo
  info "$(t "Journal" "Logs")${DP}${dim}journalctl -u agent-hub-lxd -f${reset}"
  info "$(t "Capacité vue par l'agent" "Capacity as the agent sees it")${DP}${dim}sudo -u $UTILISATEUR $BIN_CIBLE status${reset}"
  echo
}

desinstaller() {
  local present_agent=0 present_modele=0 present_lxd=0 i
  [[ -f "$UNIT" || -x "$BIN_CIBLE" || -f "$CONF" ]] && present_agent=1
  lxd_present && present_lxd=1
  ((present_lxd)) && [[ -n "$(etat_vm "$MODELE" "$PROJET")" ]] && present_modele=1

  if ((!INSTALLER_AGENT && !INSTALLER_MODELE && !INSTALLER_LXD)); then
    local -a opts=() codes=()
    ((present_agent)) && { opts+=("Agent    ${dim}$(t "service, et avec --purge binaire, configuration, enrôlement" "service, and with --purge binary, configuration, enrollment")${reset}"); codes+=(agent); }
    ((present_modele)) && { opts+=("$(t "Modèle" "Template")   ${dim}$(t "la VM $MODELE" "the $MODELE VM")${reset}"); codes+=(modele); }
    ((present_lxd && PURGER)) && { opts+=("LXD      ${dim}$(t "le snap LXD et TOUTES ses instances" "the LXD snap and ALL its instances")${reset}"); codes+=(lxd); }
    ((${#opts[@]})) || { info "$(t "Rien à désinstaller." "Nothing to uninstall.")"; return 0; }
    interactif || die "$(t "préciser les composants : --agent, --template ou --lxd (voir --help)." "specify the components: --agent, --template or --lxd (see --help).")"
    cocher "$(t "Que faut-il désinstaller ?" "What should be uninstalled?")" "${opts[@]}" || die "$(t "abandon." "aborted.")"
    for i in "${COCHES[@]}"; do
      case "${codes[$i]}" in agent) INSTALLER_AGENT=1 ;; modele) INSTALLER_MODELE=1 ;; lxd) INSTALLER_LXD=1 ;; esac
    done
  fi
  ((INSTALLER_AGENT || INSTALLER_MODELE || INSTALLER_LXD)) || { info "$(t "Rien de coché." "Nothing checked.")"; return 0; }

  if ((INSTALLER_AGENT)); then
    etape "Agent"
    if [[ -f "$UNIT" ]]; then
      systemctl disable --now agent-hub-lxd >/dev/null 2>&1 || true
      rm -f "$UNIT"; systemctl daemon-reload
      ok "$(t "Service retiré" "Service removed")"
    fi
    if ((PURGER)); then
      rm -f "$BIN_CIBLE"; rm -rf "$CONF_DIR" "$ETAT_DIR"
      id "$UTILISATEUR" >/dev/null 2>&1 && userdel "$UTILISATEUR" 2>/dev/null || true
      ok "$(t "Binaire, configuration, enrôlement et utilisateur retirés" "Binary, configuration, enrollment and user removed")"
      warn "$(t "Révoquer aussi ce serveur sur le hub (Cloud servers), si ce n'est pas fait." "Also revoke this server on the hub (Cloud servers), if not done yet.")"
    else
      info "${dim}$(t "Binaire, configuration et enrôlement gardés (--purge pour les retirer)." "Binary, configuration and enrollment kept (--purge removes them).")${reset}"
    fi
  fi

  if ((INSTALLER_MODELE)) && lxd_present && [[ -n "$(etat_vm "$MODELE" "$PROJET")" ]]; then
    etape "$(t "VM modèle" "Template VM")"
    confirmer_grave "$(t "La VM $MODELE (projet $PROJET) sera supprimée ; les instances déjà créées restent." "The $MODELE VM (project $PROJET) will be deleted; instances already created stay.")" "$MODELE"
    lxc delete --force "$MODELE" --project "$PROJET"
    ok "$(t "VM modèle supprimée" "Template VM deleted")"
  fi

  if ((INSTALLER_LXD)) && lxd_present; then
    ((PURGER)) || die "$(t "retirer LXD demande --purge : toutes ses instances seraient détruites." "removing LXD requires --purge: all its instances would be destroyed.")"
    etape "LXD"
    confirmer_grave "$(t "Le snap LXD et TOUTES ses instances, images et volumes seront détruits." "The LXD snap and ALL its instances, images and volumes will be destroyed.")" "$(t "DETRUIRE" "DESTROY")"
    suivre "$(t "Retrait du snap LXD" "Removing the LXD snap")" snap remove --purge lxd
  fi
}

# ================================================================== Démarrage

if [[ "$MODE" == aide ]]; then aide; exit 0; fi
command -v systemctl >/dev/null || die "$(t "systemd est nécessaire (systemctl introuvable)." "systemd is required (systemctl not found).")"
if [[ "$MODE" == etat ]]; then afficher_etat; exit 0; fi

# Droits administrateur : on propose de relancer avec sudo (la langue choisie suit).
if [[ $EUID -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1 && interactif &&
    oui_non "$(t "Cette opération demande les droits administrateur. Relancer avec sudo ?" "This requires administrator rights. Run again with sudo?")"; then
    exec sudo env OPENCODE_HUB_SANS_BANNIERE=1 OPENCODE_HUB_LANG="$LANGUE" bash "$0" "${ARGS_ORIGINAUX[@]}"
  fi
  die "$(t "lancer en administrateur" "run as administrator")${DP}sudo $0 ${ARGS_ORIGINAUX[*]}"
fi

APPELANT="${SUDO_USER:-root}"

if [[ "$MODE" == desinstaller ]]; then desinstaller; exit 0; fi

# ------------------------------------------------------------------ Choix des composants

if ((!INSTALLER_LXD && !INSTALLER_MODELE && !INSTALLER_AGENT)); then
  interactif || die "$(t "préciser au moins un composant : --lxd, --template ou --agent (voir --help)." "specify at least one component: --lxd, --template or --agent (see --help).")"
  if lxd_initialise; then lxd_info="$(t "déjà installé : projet et réseau seulement" "already installed: project and network only")"
  elif lxd_present; then lxd_info="$(t "installé, à initialiser" "installed, to initialize")"
  else lxd_info="$(t "installation par snap et initialisation" "snap install and initialization")"; fi
  cocher "$(t "Que faut-il installer ?" "What should be installed?")" \
    "LXD      ${dim}$lxd_info${reset}" \
    "$(t "Modèle" "Template")   ${dim}$(t "VM clonée pour chaque instance Cloud" "VM cloned for each cloud instance")${reset}" \
    "Agent    ${dim}$(t "relie ce serveur à un hub" "connects this server to a hub")${reset}" || die "$(t "abandon." "aborted.")"
  for i in "${COCHES[@]}"; do
    case "$i" in 0) INSTALLER_LXD=1 ;; 1) INSTALLER_MODELE=1 ;; 2) INSTALLER_AGENT=1 ;; esac
  done
  ((INSTALLER_LXD || INSTALLER_MODELE || INSTALLER_AGENT)) || die "$(t "aucun composant coché : rien à faire." "no component checked: nothing to do.")"
fi
choix=()
((INSTALLER_LXD)) && choix+=("LXD")
((INSTALLER_MODELE)) && choix+=("$(t "Modèle" "Template")")
((INSTALLER_AGENT)) && choix+=("Agent")
ok "$(t "Composants" "Components")${DP}${bold}${choix[*]}${reset}"

# Le modèle et l'agent s'appuient sur un LXD initialisé, que ce passage-ci l'installe ou non.
if ((INSTALLER_MODELE || INSTALLER_AGENT)) && ((!INSTALLER_LXD)) && ! lxd_initialise; then
  die "$(t "LXD n'est pas installé ou pas initialisé : cocher aussi LXD (--lxd)." "LXD is not installed or not initialized: also check LXD (--lxd).")"
fi
if ((INSTALLER_LXD)) && ! lxd_present; then
  command -v snap >/dev/null || command -v apt-get >/dev/null ||
    die "$(t "l'installation de LXD demande snap (Ubuntu) ou apt-get." "installing LXD requires snap (Ubuntu) or apt-get.")"
fi
if ((INSTALLER_MODELE)) && ! kvm_disponible; then
  die "$(t "/dev/kvm absent : ce serveur ne peut pas faire tourner de VM (activer la virtualisation matérielle)." "/dev/kvm missing: this server cannot run VMs (enable hardware virtualization).")"
fi
kvm_disponible || warn "$(t "/dev/kvm absent : LXD ne pourra pas démarrer les VM des instances Cloud." "/dev/kvm missing: LXD will not be able to start the cloud instances' VMs.")"

# ------------------------------------------------------------------ Réglages de LXD

if ((INSTALLER_LXD)); then
  etape "$(t "Réglages de LXD" "LXD settings")"
  if ! lxd_initialise; then
    if [[ -z "$STOCKAGE" ]]; then
      menu "$(t "Stockage des VM" "VM storage")" \
        "zfs ${dim}$(t "(recommandé : clones instantanés du modèle)" "(recommended: instant clones of the template)")${reset}" \
        "dir ${dim}$(t "(simple, copie complète à chaque instance)" "(simple, full copy for each instance)")${reset}" || die "$(t "abandon." "aborted.")"
      if ((MENU_CHOIX == 1)); then STOCKAGE=dir; else STOCKAGE=zfs; fi
    fi
    [[ "$STOCKAGE" == zfs || "$STOCKAGE" == dir ]] || die "--storage${DP}zfs $(t "ou" "or") dir"
    POOL="${POOL:-default}"
  else
    POOL="${POOL:-$(premier_pool)}"
  fi
fi
if ((INSTALLER_LXD || INSTALLER_MODELE || INSTALLER_AGENT)); then
  demander PROJET "$(t "Projet LXD des instances Cloud" "LXD project of the cloud instances")" "hub" "$RE_NOM_LXD" "$(t "lettres, chiffres, . _ -" "letters, digits, . _ -")"
  if lxd_initialise; then POOL="${POOL:-$(premier_pool)}"; fi
  demander POOL "$(t "Pool de stockage des disques" "Storage pool of the disks")" "${POOL:-default}" "$RE_NOM_LXD" "$(t "lettres, chiffres, . _ -" "letters, digits, . _ -")"
fi
if ((INSTALLER_LXD)) && [[ -z "$ISOLER" ]]; then
  if oui_non "$(t "Réseau isolé ${bold}$RESEAU${reset} : les VM accèdent à Internet mais ni à cet hôte ni au réseau local ?" "Isolated ${bold}$RESEAU${reset} network: VMs reach the Internet but neither this host nor the local network?")"; then ISOLER=1; else ISOLER=0; fi
fi
ISOLER="${ISOLER:-0}"

# ------------------------------------------------------------------ Réglages du modèle

if ((INSTALLER_MODELE)); then
  etape "$(t "Réglages de la VM modèle" "Template VM settings")"
  demander MODELE "$(t "Nom de la VM modèle" "Template VM name")" "opencode-template" "$RE_NOM_LXD" "$(t "lettres, chiffres, . _ -" "letters, digits, . _ -")"
  demander IMAGE "$(t "Image de départ" "Base image")" "ubuntu:24.04" "$RE_IMAGE" "$(t "une image LXD, par exemple ubuntu:24.04" "an LXD image, for example ubuntu:24.04")"
  if lxd_present && [[ -n "$(etat_vm "$MODELE" "$PROJET")" ]] && ((!REFAIRE_MODELE)); then
    if oui_non "$(t "La VM $MODELE existe déjà. La recréer ?" "The $MODELE VM already exists. Recreate it?")" n; then REFAIRE_MODELE=1; fi
  fi
  if [[ -z "$OPENCODE_BINAIRE" ]]; then
    candidats=()
    for c in /usr/local/bin/opencode "$(command -v opencode 2>/dev/null || true)" "$(dossier_de "$APPELANT")/.opencode/bin/opencode"; do
      [[ -n "$c" && -x "$c" ]] || continue
      c="$(readlink -f "$c")"
      [[ " ${candidats[*]} " == *" $c "* ]] || candidats+=("$c")
    done
    options=()
    for c in "${candidats[@]}"; do options+=("$(t "Utiliser" "Use") $c ${dim}($(version_de "$c"))${reset}"); done
    options+=("$(t "Indiquer le chemin d'un binaire" "Enter the path of a binary")")
    info "$(t "La VM a besoin du binaire ${bold}opencode-hub${reset} (opencode avec « agent join ») ; son installateur le construit (./build.sh)." "The VM needs the ${bold}opencode-hub${reset} binary (opencode with \"agent join\"); its installer builds it (./build.sh).")"
    if ! interactif && ((${#candidats[@]} == 0)); then
      die "$(t "aucun binaire opencode : passer --opencode-binary <chemin>." "no opencode binary: pass --opencode-binary <path>.")"
    fi
    menu "$(t "Quel binaire opencode installer dans la VM ?" "Which opencode binary should go into the VM?")" "${options[@]}" || die "$(t "abandon." "aborted.")"
    if ((MENU_CHOIX < ${#candidats[@]})); then OPENCODE_BINAIRE="${candidats[$MENU_CHOIX]}"
    else demander OPENCODE_BINAIRE "$(t "Chemin du binaire opencode-hub" "Path of the opencode-hub binary")"; fi
  fi
  [[ -f "$OPENCODE_BINAIRE" ]] || die "$(t "binaire introuvable" "binary not found")${DP}$OPENCODE_BINAIRE"
  OPENCODE_BINAIRE="$(readlink -f "$OPENCODE_BINAIRE")"
  if ! "$OPENCODE_BINAIRE" agent --help 2>&1 | grep -q join; then
    warn "$(t "$OPENCODE_BINAIRE ne semble pas connaître « agent join » : la VM ne pourra pas rejoindre le hub. Prendre le binaire d'opencode-hub." "$OPENCODE_BINAIRE does not seem to know \"agent join\": the VM could not join the hub. Use the opencode-hub binary.")"
    oui_non "$(t "Continuer quand même ?" "Continue anyway?")" n || die "$(t "abandon." "aborted.")"
  fi
  for f in hub-pair opencode-agent.service; do
    [[ -f "$REPO/template/$f" ]] || die "$(t "fichier du modèle introuvable" "template file not found")${DP}$REPO/template/$f"
  done
fi

# ------------------------------------------------------------------ Réglages de l'agent

if ((INSTALLER_AGENT)); then
  etape "$(t "Réglages de l'agent" "Agent settings")"
  [[ -n "$HUB_URL" ]] || HUB_URL="$(hub_enrole)"
  while :; do
    demander HUB_URL "$(t "URL du hub à rejoindre" "URL of the hub to join")" "" "$RE_URL" "$(t "une URL du type https://hub.example.com" "a URL such as https://hub.example.com")"
    HUB_URL="${HUB_URL%/}"
    url_sure "$HUB_URL" && break
    if ((INSECURE)); then
      warn "$(t "--insecure : le code et le jeton de ce serveur passeront en clair vers $HUB_URL." "--insecure: this server's code and token will travel in clear text to $HUB_URL.")"
      break
    fi
    err "$(t "$HUB_URL est en http:// sur une autre machine : le code et le jeton passeraient en clair. Utiliser https:// (ou --insecure)." "$HUB_URL is plain http:// on another machine: the code and the token would travel in clear text. Use https:// (or --insecure).")"
    interactif || exit 1
    HUB_URL=""
  done
  demander NOM_HOTE "$(t "Nom de ce serveur sur le hub" "Name of this server on the hub")" "$(hostname)"
  [[ "$MODELE" =~ $RE_NOM_LXD ]] || die "$(t "nom de modèle invalide" "invalid template name")${DP}$MODELE"
  if [[ -z "$BINARY" ]] && ((!CONSTRUIRE)); then
    options=()
    ((${#BIN_CIBLE} > 0)) && [[ -x "$BIN_CIBLE" ]] && options+=("$(t "Garder" "Keep") $BIN_CIBLE ${dim}($(version_de "$BIN_CIBLE"))${reset}")
    [[ -f "$REPO/Cargo.toml" ]] && options+=("$(t "Construire depuis ce dépôt" "Build from this repository") ${dim}$(t "(cargo, installé au besoin)" "(cargo, installed if needed)")${reset}")
    options+=("$(t "Indiquer le chemin d'un binaire" "Enter the path of a binary")")
    menu "$(t "Quel binaire agent-hub-lxd installer ?" "Which agent-hub-lxd binary should be installed?")" "${options[@]}" || die "$(t "abandon." "aborted.")"
    choisi="${options[$MENU_CHOIX]}"
    case "$choisi" in
      "$(t "Garder" "Keep")"*) BINARY="$BIN_CIBLE" ;;
      "$(t "Construire" "Build")"*) CONSTRUIRE=1 ;;
      *) demander BINARY "$(t "Chemin du binaire agent-hub-lxd" "Path of the agent-hub-lxd binary")" ;;
    esac
  fi
  if [[ -n "$BINARY" ]]; then
    [[ -f "$BINARY" ]] || die "$(t "--binary introuvable" "--binary not found")${DP}$BINARY"
    BINARY="$(readlink -f "$BINARY")"
  fi
  if [[ -z "$SERVICE" ]]; then
    if oui_non "$(t "Installer le service systemd ${bold}agent-hub-lxd${reset} avec démarrage automatique ?" "Install the ${bold}agent-hub-lxd${reset} systemd service, started at boot?")"; then SERVICE=1; else SERVICE=0; fi
  fi
fi
SERVICE="${SERVICE:-0}"

# ------------------------------------------------------------------ Récapitulatif

oui_txt() { if [[ "$1" == 1 ]]; then printf '%s%s%s' "$green" "$(t "oui" "yes")" "$reset"; else printf '%s%s%s' "$dim" "$(t "non" "no")" "$reset"; fi; }
recap=()
if ((INSTALLER_LXD)); then
  if lxd_initialise; then recap+=("${bold}LXD${reset}     $(t "déjà initialisé" "already initialized") ${dim}$S_SEP pool $POOL${reset}")
  elif lxd_present; then recap+=("${bold}LXD${reset}     $(t "initialisation" "initialization") ${dim}$S_SEP $STOCKAGE${reset}")
  else recap+=("${bold}LXD${reset}     $(t "installation par snap et initialisation" "snap install and initialization") ${dim}$S_SEP $STOCKAGE${reset}"); fi
  recap+=("        $(t "projet" "project") $PROJET ${dim}$S_SEP $(t "réseau isolé" "isolated network")${DP}${reset}$(oui_txt "$ISOLER")")
fi
if ((INSTALLER_MODELE)); then
  recap+=("${bold}$(t "Modèle" "Template")${reset}  $MODELE ${dim}$S_SEP $IMAGE $S_SEP $(t "recréé" "recreated")${DP}${reset}$(oui_txt "$REFAIRE_MODELE")")
  recap+=("        opencode${DP}${dim}$OPENCODE_BINAIRE${reset}")
fi
if ((INSTALLER_AGENT)); then
  recap+=("${bold}Agent${reset}   $HUB_URL ${dim}$S_SEP $NOM_HOTE $S_SEP $(t "utilisateur" "user") $UTILISATEUR${reset}")
  if ((CONSTRUIRE)); then recap+=("        $(t "binaire construit depuis $REPO" "binary built from $REPO")")
  else recap+=("        $(t "binaire" "binary")${DP}${dim}$BINARY${reset}"); fi
  recap+=("        $(t "service systemd" "systemd service")${DP}$(oui_txt "$SERVICE")")
fi
echo
cadre "$gris" "$(t "Récapitulatif" "Summary")" "${recap[@]}"
oui_non "$(t "Lancer l'installation ?" "Start the installation?")" || die "$(t "abandon : rien n'a été modifié." "aborted: nothing was changed.")"

# ================================================================== Installation

# ------------------------------------------------------------------ LXD

attendre_lxd() { lxd waitready --timeout=120; }

if ((INSTALLER_LXD)); then
  etape "LXD"
  if ! lxd_present; then
    if ! command -v snap >/dev/null; then suivre "$(t "Installation de snapd" "Installing snapd")" apt_installer snapd; fi
    suivre "$(t "Installation du snap LXD" "Installing the LXD snap")" snap install lxd
  else
    ok "$(t "LXD déjà installé" "LXD already installed")${DP}$(lxc version 2>/dev/null | sed -n 's/^Server version: //p')"
  fi
  suivre "$(t "Attente du démon LXD" "Waiting for the LXD daemon")" attendre_lxd
  if ! lxd_initialise; then
    initialiser() { lxd init --auto --storage-backend "$STOCKAGE"; }
    suivre "$(t "Initialisation de LXD ($STOCKAGE)" "Initializing LXD ($STOCKAGE)")" initialiser
    POOL="$(premier_pool)"
  fi
  lxc storage show "$POOL" >/dev/null 2>&1 || die "$(t "pool de stockage introuvable" "storage pool not found")${DP}$POOL"

  # Le groupe qui ouvre le socket de LXD ; l'agent en fait partie.
  if ! getent group lxd >/dev/null; then
    groupadd --system lxd
    suivre "$(t "Redémarrage de LXD pour le groupe lxd" "Restarting LXD for the lxd group")" snap restart lxd
    suivre "$(t "Attente du démon LXD" "Waiting for the LXD daemon")" attendre_lxd
  fi

  if ((ISOLER)); then
    if ! lxc network show "$RESEAU" >/dev/null 2>&1; then
      lxc network create "$RESEAU" ipv4.address=auto ipv4.nat=true ipv6.address=none >/dev/null
      ok "$(t "Réseau créé" "Network created")${DP}$RESEAU"
    fi
    if ! lxc network acl show "$ACL" >/dev/null 2>&1; then
      lxc network acl create "$ACL" >/dev/null
      # Vers les réseaux privés, l'hôte compris : refusé. Le reste (Internet) passe ; LXD laisse DHCP et DNS du pont.
      for plage in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 100.64.0.0/10; do
        lxc network acl rule add "$ACL" egress action=reject destination="$plage" >/dev/null
      done
      ok "$(t "Règles d'isolation créées" "Isolation rules created")${DP}$ACL"
    fi
    lxc network set "$RESEAU" security.acls="$ACL" security.acls.default.egress.action=allow \
      security.acls.default.ingress.action=allow
  fi

  if ! projet_existe "$PROJET"; then
    lxc project create "$PROJET" -c features.images=false -c features.profiles=true -c features.storage.volumes=true \
      -c restricted=true -c restricted.virtual-machines.lowlevel=block >/dev/null
    ok "$(t "Projet créé" "Project created")${DP}$PROJET"
  else
    ok "$(t "Projet déjà présent" "Project already exists")${DP}$PROJET"
  fi
  lxc profile device show default --project "$PROJET" 2>/dev/null | grep -q '^root:' ||
    lxc profile device add default root disk path=/ pool="$POOL" --project "$PROJET" >/dev/null
  lxc profile device show default --project "$PROJET" 2>/dev/null | grep -q '^eth0:' || {
    reseau_vm="$RESEAU"; ((ISOLER)) || reseau_vm="$(lxc network list -f csv 2>/dev/null | awk -F, '$2=="bridge" && $3=="YES"{print $1; exit}')"
    [[ -n "$reseau_vm" ]] || reseau_vm=lxdbr0
    lxc profile device add default eth0 nic network="$reseau_vm" name=eth0 --project "$PROJET" >/dev/null
  }
  ok "$(t "Profil du projet prêt" "Project profile ready")${DP}pool $POOL"
fi

# ------------------------------------------------------------------ VM modèle

dans_modele() { lxc exec "$MODELE" --project "$PROJET" -- "$@"; }

attendre_modele() {
  local _
  for _ in $(seq 1 90); do
    dans_modele true >/dev/null 2>&1 && return 0
    sleep 2
  done
  return 1
}

preparer_modele() {
  # cloud-init termine la mise en route (réseau, paquets) avant toute modification.
  dans_modele cloud-init status --wait >/dev/null 2>&1 || true
  dans_modele env DEBIAN_FRONTEND=noninteractive apt-get update -q
  dans_modele env DEBIAN_FRONTEND=noninteractive apt-get install -y -q git curl ca-certificates
  dans_modele bash -c 'id opencode >/dev/null 2>&1 || useradd -m -s /bin/bash opencode'
  lxc file push "$OPENCODE_BINAIRE" "$MODELE/usr/local/bin/opencode" --project "$PROJET" --mode 0755 --uid 0 --gid 0
  lxc file push "$REPO/template/hub-pair" "$MODELE$APPAIRAGE" --project "$PROJET" --mode 0755 --uid 0 --gid 0
  lxc file push "$REPO/template/opencode-agent.service" "$MODELE/etc/systemd/system/opencode-agent.service" \
    --project "$PROJET" --mode 0644 --uid 0 --gid 0
  dans_modele systemctl daemon-reload
  dans_modele runuser -u opencode -- /usr/local/bin/opencode --version
}

# Ce que chaque clone partagerait : identité machine, clés SSH, enrôlement, journaux. Régénérés au démarrage d'un clone.
nettoyer_modele() {
  dans_modele bash -c '
    systemctl disable opencode-agent.service >/dev/null 2>&1 || true
    rm -f /home/opencode/.local/share/opencode/agent.json /run/agent-hub-lxd-pairing
    rm -rf /home/opencode/.local/share/opencode/hub-instances
    apt-get clean
    cloud-init clean --logs --machine-id >/dev/null 2>&1 || true
    truncate -s 0 /etc/machine-id
    rm -f /var/lib/dbus/machine-id /etc/ssh/ssh_host_*
    journalctl --rotate >/dev/null 2>&1 || true
    journalctl --vacuum-time=1s >/dev/null 2>&1 || true
    history -c || true
  '
}

arreter_modele() { lxc stop "$MODELE" --project "$PROJET" --timeout 60 || lxc stop "$MODELE" --project "$PROJET" --force; }

if ((INSTALLER_MODELE)); then
  etape "$(t "VM modèle" "Template VM")"
  if [[ -n "$(etat_vm "$MODELE" "$PROJET")" ]]; then
    if ((REFAIRE_MODELE)); then
      lxc delete --force "$MODELE" --project "$PROJET"
      ok "$(t "Ancienne VM modèle supprimée" "Previous template VM deleted")"
    else
      ok "$(t "VM modèle déjà présente, gardée" "Template VM already exists, kept")${DP}$MODELE"
      INSTALLER_MODELE=0
    fi
  fi
fi
if ((INSTALLER_MODELE)); then
  # Petit disque : les instances l'agrandissent à leur taille, mais ne peuvent pas descendre en dessous.
  lancer() {
    lxc launch "$IMAGE" "$MODELE" --vm --project "$PROJET" -s "$POOL" -c limits.cpu=2 -c limits.memory=2GiB \
      -d root,size=10GiB
  }
  suivre "$(t "Création de la VM $MODELE ($IMAGE)" "Creating the $MODELE VM ($IMAGE)")" lancer
  suivre "$(t "Attente de l'agent LXD de la VM" "Waiting for the VM's LXD agent")" attendre_modele ||
    die "$(t "la VM ne répond pas" "the VM does not answer")${DP}lxc console $MODELE --project $PROJET"
  suivre "$(t "Installation d'opencode-hub dans la VM" "Installing opencode-hub in the VM")" preparer_modele
  suivre "$(t "Nettoyage de l'identité de la VM" "Clearing the VM's identity")" nettoyer_modele
  suivre "$(t "Arrêt de la VM modèle" "Stopping the template VM")" arreter_modele
  ok "$(t "VM modèle prête" "Template VM ready")${DP}$MODELE $S_SEP $(t "projet" "project") $PROJET"
fi

# ------------------------------------------------------------------ Agent

installer_rust() {
  apt_installer build-essential pkg-config curl ca-certificates
  en_tant_que "$APPELANT" bash -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal'
}
construire() { en_tant_que "$APPELANT" bash -c 'cd "$1" && cargo build --release --locked' _ "$REPO"; }

attendre_agent() {
  local _
  for _ in $(seq 1 20); do
    systemctl is-active --quiet agent-hub-lxd && return 0
    sleep 0.5
  done
  return 1
}

if ((INSTALLER_AGENT)); then
  etape "Agent"
  if ((CONSTRUIRE)); then
    if ! en_tant_que "$APPELANT" bash -c 'command -v cargo' >/dev/null 2>&1; then
      oui_non "$(t "cargo est absent. Installer Rust (rustup) pour $APPELANT ?" "cargo is missing. Install Rust (rustup) for $APPELANT?")" ||
        die "$(t "cargo est nécessaire pour construire l'agent (ou passer --binary)." "cargo is required to build the agent (or pass --binary).")"
      suivre "$(t "Installation de Rust" "Installing Rust")" installer_rust
    fi
    suivre "$(t "Construction d'agent-hub-lxd (cargo build --release)" "Building agent-hub-lxd (cargo build --release)")" construire
    BINARY="$REPO/target/release/agent-hub-lxd"
    [[ -x "$BINARY" ]] || die "$(t "binaire introuvable après la construction" "binary not found after the build")${DP}$BINARY"
  fi
  if [[ "$BINARY" != "$BIN_CIBLE" ]]; then
    install -m 0755 "$BINARY" "$BIN_CIBLE.new"
    mv -f "$BIN_CIBLE.new" "$BIN_CIBLE"
    ok "$(t "Binaire installé" "Binary installed")${DP}$BIN_CIBLE"
  else
    ok "$(t "Binaire déjà en place" "Binary already in place")${DP}$BIN_CIBLE"
  fi
  info "Version${DP}$(version_de "$BIN_CIBLE")"

  # Utilisateur dédié, dans le groupe lxd : l'accès à LXD équivaut à root sur l'hôte, rien d'autre n'est ouvert.
  if id "$UTILISATEUR" >/dev/null 2>&1; then
    ok "$(t "Utilisateur $UTILISATEUR déjà présent" "User $UTILISATEUR already exists")"
  else
    useradd --system --no-create-home --home-dir "$ETAT_DIR" --shell /usr/sbin/nologin "$UTILISATEUR"
    ok "$(t "Utilisateur système créé" "System user created")${DP}$UTILISATEUR"
  fi
  usermod -aG lxd "$UTILISATEUR"
  install -d -m 0700 -o "$UTILISATEUR" -g "$UTILISATEUR" "$ETAT_DIR"
  install -d -m 0750 -o root -g "$UTILISATEUR" "$CONF_DIR"

  if [[ -f "$CONF" ]] && ! grep -qF "$MARQUE" "$CONF"; then
    ok "$(t "Configuration écrite à la main gardée" "Hand-written configuration kept")${DP}$CONF"
  else
    cat >"$CONF" <<EOF
$MARQUE
# Toutes les clés : $REPO/packaging/config.example.toml
name = "$NOM_HOTE"

[lxd]
project = "$PROJET"
template = "$MODELE"
storage_pool = "$POOL"
pair_command = ["$APPAIRAGE"]
EOF
    chown root:"$UTILISATEUR" "$CONF"
    chmod 0640 "$CONF"
    ok "$(t "Configuration écrite" "Configuration written")${DP}$CONF"
  fi

  # Environnement propre : rien de celui de root (variables, certificats, proxy) ne passe à l'agent.
  agent() {
    runuser -u "$UTILISATEUR" -- env -i HOME="$ETAT_DIR" LANG="${LANG:-C.UTF-8}" PATH=/usr/local/bin:/usr/bin:/bin \
      "$BIN_CIBLE" --config "$CONF" --state-dir "$ETAT_DIR" "$@"
  }
  verifier() { agent status; }
  suivre "$(t "Vérification de LXD, du projet et du modèle" "Checking LXD, the project and the template")" verifier ||
    warn "$(t "l'agent ne trouve pas encore tout ce qu'il lui faut (voir ci-dessus) ; il refusera de démarrer." "the agent does not find everything it needs yet (see above); it will refuse to start.")"

  enroler=1
  if [[ -f "$ETAT_DIR/agent.json" ]]; then
    info "$(t "Serveur déjà enrôlé auprès de" "Server already enrolled with") $(hub_enrole)"
    if [[ -z "$CODE" ]] && ! oui_non "$(t "L'enrôler de nouveau ?" "Enroll it again?")" n; then enroler=0; fi
  fi
  if ((enroler)); then
    if [[ -z "$CODE" ]]; then
      info "$(t "Code du serveur : tableau de bord" "Server code: dashboard") $(lien "$HUB_URL"), ${bold}Cloud servers${reset} $S_SEP ${bold}Link server${reset} $(t "(administrateur)" "(administrator)")."
      demander_secret CODE "$(t "Code de serveur Cloud" "Cloud server code")"
    fi
    # Le code passe par l'entrée standard : il n'apparaît ni dans la liste des processus ni dans un journal.
    rejoindre() {
      local -a opts=(--name "$NOM_HOTE" --force)
      ((INSECURE)) && opts+=(--insecure)
      printf '%s\n' "$CODE" | agent join "$HUB_URL" "${opts[@]}"
    }
    suivre "$(t "Enrôlement auprès de" "Enrolling with") $HUB_URL" rejoindre ||
      die "$(t "enrôlement refusé : vérifier l'URL et le code (un code ne sert qu'une fois)." "enrollment refused: check the URL and the code (a code works only once).")"
  fi

  if ((SERVICE)); then
    {
      echo "$MARQUE"
      sed -e "s#^ExecStart=.*#ExecStart=$BIN_CIBLE --config $CONF --state-dir $ETAT_DIR run#" \
        -e "s#^User=.*#User=$UTILISATEUR#" -e "s#^Group=.*#Group=$UTILISATEUR#" "$REPO/packaging/agent-hub-lxd.service"
    } >"$UNIT"
    ok "$(t "Service écrit" "Service written")${DP}$UNIT"
    systemctl daemon-reload
    systemctl enable agent-hub-lxd >/dev/null 2>&1
    systemctl restart agent-hub-lxd
    if suivre "$(t "Démarrage de l'agent" "Starting the agent")" attendre_agent; then
      ok "$(t "Agent en marche, démarrage automatique activé" "Agent running, start at boot enabled")"
    else
      warn "$(t "l'agent ne tourne pas" "the agent is not running")${DP}journalctl -u agent-hub-lxd -e"
    fi
  else
    info "$(t "Sans service : garder ce serveur relié avec" "Without a service: keep this server connected with")"
    echo "  sudo -u $UTILISATEUR $BIN_CIBLE --config $CONF --state-dir $ETAT_DIR run"
  fi
fi

# ================================================================== Résumé

fin=()
((INSTALLER_AGENT)) && fin+=("$(pad "Hub" 16) $(lien "$HUB_URL") ${dim}$S_SEP Cloud servers${reset}")
((INSTALLER_AGENT && SERVICE)) && fin+=("$(pad "$(t "Journal agent" "Agent logs")" 16) ${dim}journalctl -u agent-hub-lxd -f${reset}")
lxd_present && fin+=("$(pad "$(t "Instances" "Instances")" 16) ${dim}lxc list --project $PROJET${reset}")
fin+=("$(pad "$(t "État" "Status")" 16) ${dim}./install.sh --status${reset}")
if ((!INSTALLER_AGENT)) && [[ ! -f "$ETAT_DIR/agent.json" ]]; then
  fin+=("" "$(t "Relier ce serveur à un hub :" "Connect this server to a hub:")")
  fin+=("  ${dim}sudo ./install.sh --agent --hub-url <url> --service${reset}")
fi
echo
cadre "$green" "$(t "Installation terminée" "Installation complete")" "${fin[@]}"
echo

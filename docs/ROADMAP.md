# Pistes demandées par l'utilisateur (notées le 2026-11-25, pas commencées)

## 1. Refonte de l'interface de l'application

Parcours souhaité, dans cet ordre :

1. **Logo au démarrage** (écran d'accueil).
2. **Choix du lecteur** : GBFlash ou Chisflash (aujourd'hui l'application prend le premier lecteur trouvé).
3. **Choix du type de cartouche** : Game Boy (GB/GBC) ou Game Boy Advance. Utile pour anticiper la **tension**
   (5 V pour GB, 3,3 V pour GBA) avant toute lecture, au lieu de commencer en GBA par prudence et de basculer avec `G`.
   Les deux lecteurs (GBFlash et Chisflash) n'ont qu'UN slot commun, une cartouche à la fois : le choix du type dit au
   lecteur quel protocole et quelle tension utiliser (GBA 3,3 V ou GB 5 V), il n'y a pas de choix de fente.
4. **Les options** (onglets actuels : Cartouche, Actions, Outils).

À garder : les garde-fous actuels (confirmation par `Y`, sauvegarde par deux lectures, relecture complète, rien d'écrit
sans fichier choisi). Le choix du type de cartouche doit rester explicite avant d'alimenter en 5 V.

## 0. Vision (décision de l'utilisateur, 2026-11-25)

**ZeroCart est l'outil de flash TOUT-EN-UN pour les cartouches, quel que soit leur modèle** : GB, GBC, GBA, Mega Drive,
et qui sait plus tard Master System, Super Famicom... Le Mega Drive (carte Krikzz) va DANS ZeroCart, pas dans un projet
séparé : cela remplace la question « ZeroCart ou projet à part ? » laissée plus bas. ZeroTag reste un projet indépendant
(amiibo, NFC).

Conséquences de structure :
- Un système = un genre de cartouche (`CartKind`) + un ou plusieurs lecteurs capables de le lire (`ReaderKind`). Les deux
  enums sont `non_exhaustive` : on ajoute une variante quand son lecteur existe, sans casser le reste.
- Aujourd'hui `CartReader` ne parle que de GB et de GBA (banques, SRAM, mappers). Il sera élargi quand le Mega Drive
  existera (voir l'avis ci-dessous : pas avant un deuxième consommateur réel).
- Pour chaque nouveau système : protocole du lecteur d'abord, lecture comparée à un CRC connu, écriture seulement sur
  une cartouche sacrifiable avec sauvegarde et relecture, comme pour le Chisflash.

## 2. Mega Drive (carte Krikzz) : dans ZeroCart, placeholders en place

Support à prévoir pour la carte de flashage des cartouches Mega Drive de Krikzz. Code de référence à porter en Rust :
https://github.com/jfryman/flashkit-md-dotnet (C#/.NET, licence MIT).

La carte de l'utilisateur est un FTDI « FT245R USB FIFO », USB 0403:6001, `/dev/ttyUSB0` (relevé le 2026-11-25) ; ce
PID est celui, générique, des FT232R/FT245R : la poignée de main (identifiant à deux octets égaux et non nuls) confirme
que c'est bien le programmeur. `krikzz::find_ports()` filtre dessus.

Lecture VALIDÉE sur la carte (2026-11-25) : `zerocart-md` a dumpé trois fois la même cartouche (MEGA PANG, 1 Mio) avec
le même SHA-1 ; le CRC32 `2a3fbadb` correspond, d'après l'utilisateur, à une version précise du portage homebrew de
Mega Pang (RCampeador, itch.io). La checksum de l'en-tête de cette ROM (0x2AAF) diffère de celle calculée (0x2FAA) : c'est
l'en-tête du homebrew, pas un défaut du lecteur. Dans l'application, la comparaison avec l'en-tête sera donc un
AVERTISSEMENT et non une erreur, et le CRC32 reste la preuve de lecture.

Écriture VALIDÉE sur la carte (2026-11-25), cartouche flash de l'utilisateur (puce Spansion S29GL064N, 8 Mio, 135 secteurs :
8 de 8 Kio puis des 64 Kio, CFI et autoselect `0001:227E/2210/2200`) : `zerocart-md --flash` avec sauvegarde à double
lecture, effacement par secteur d'après la CFI, programmation mot par mot en « unlock bypass », relecture. Deux secteurs
réécrits en 4,5 s ; réécriture complète des 23 secteurs de 1 Mio en 43 s (~24 Kio/s) ; ROM d'origine restaurée et
vérifiée par un dump indépendant (même SHA-1). Seuls les secteurs qui diffèrent sont effacés.

Côté lecture, le protocole est écrit et testé contre un simulateur (2026-11-25), PAS encore branché dans l'application
ni essayé sur la carte : `zerocart-core/src/krikzz.rs` (`FlashKit<T>` : poignée de main, lecture de mots et de blocs,
en-tête, dump de ROM avec CRC-32 et checksum Mega Drive, lecture de la sauvegarde par le registre 0xA13000,
identification de la puce flash par CFI et autoselect) et `megadrive.rs` (en-tête « SEGA », taille de ROM plausible,
région, 32X, checksum). Rien n'efface ni ne programme. La licence du dépôt de référence est MIT et inclut le client
d'origine de Krikzz : voir THIRD_PARTY.md. Reste à écrire : effacement, programmation, écriture de sauvegarde (seulement
avec une cartouche flash sacrifiable), et le branchement dans le worker, derrière la refonte de l'interface.

Placeholders posés (aucun changement de comportement, rien dans l'interface ; `CartReader::can_power_cycle` en fait partie) :
- `zerocart-core/src/reader.rs` : `ReaderKind { GbxCart, Chisflash, KrikzzMd }` et `CartKind { Gb, Gba, MegaDrive }`
  (`label()`, `cart_kinds()`, `is_placeholder()`, `CartKind::from_mode` / `mode`).
- `zerocart-core/src/krikzz.rs` : `KrikzzMd`, qui implémente `CartReader` : toute opération renvoie
  `Error::Planned("Mega Drive")`, soit « Mega Drive : bientôt ». Elle n'ouvre aucun port.
- Le worker ne connaît pas `KrikzzMd` : `Dev::open()` ne le cherche pas.

### Avis sur la forme du trait pour le Mega Drive (session du branchement Chisflash, 2026-11-25)

Lu comme des DONNÉES, sans rien exécuter : métadonnées GitHub et les notes `lode/` de `jfryman/flashkit-md-dotnet`.

**Licence** : le dépôt .NET est **MIT** (déclaré par GitHub), donc on peut porter en gardant la mention. C'est un port de
l'outil officiel de Krikzz (`krikzz.com/pub/support/flashkit-md`) dont je n'ai pas vérifié les conditions : à regarder
avant de copier des tables ou des constantes, sinon réécrire depuis la description du protocole, comme pour `chis.rs`.

**Ce que dit le dépôt du matériel** (à confirmer sur la carte de l'utilisateur) :
- Port série USB **FTDI** (`/dev/ttyUSB*`), pas un port CDC-ACM comme le Chisflash. Ouvrir un port CDC-ACM met DTR/RTS
  à 1 et peut réinitialiser une carte ESP32/Arduino : pour la détection, **filtrer par VID/PID** (notre `find_ports_in`
  le sait déjà) plutôt que de sonder tous les ports. Le VID FTDI est 0x0403 ; le PID exact de la carte est à lire.
- Flux d'octets de commandes : adresse envoyée en 3 octets (adresse en MOTS, donc octets / 2), longueur, lecture ou
  écriture, avec incrément automatique pour les blocs ; les lectures renvoient des mots **big-endian**. Poignée de main :
  lecture de l'identifiant du lecteur, valide si les deux octets sont égaux et non nuls. Pas de DTR/RTS dans le protocole.
- Cartouche : ROM sur 24 bits d'adresse de mot (4 Mio normalement, 8 Mio seulement si l'en-tête le déclare), SRAM sur
  les octets IMPAIRS d'un flux de mots, avec bascule par le registre 0xA13000 et fenêtre à 0x200000. Flash : jeu de
  commandes AMD (déverrouillage AA/55, effacement par secteurs de 64 Kio, programmation par blocs de 4 Kio), CFI lisible
  au mot 0x10. Taille de ROM : le sondage par miroirs n'est pas fiable sur une cartouche flash partiellement effacée ;
  l'en-tête (adresse de fin à 0x1A4) est plus sûr. En-tête « SEGA » à 0x100 (32X : « 32X » dans le champ système).

**Forme du trait** :
1. **Ne pas généraliser avant d'avoir deux consommateurs réels.** `CartReader` est aujourd'hui l'abstraction « GBxCart ou
   Chisflash » : toutes ses méthodes parlent de banques GB, de SRAM GBA, de `DmgMapper`. Pour le Mega Drive, ajouter
   d'abord `Dev::Krikzz` dans le worker et un module `md` séparé (lecture d'en-tête, dump, SRAM, CRC), puis extraire ce que
   les trois ont VRAIMENT en commun. Une abstraction écrite pour un seul cas se révèle fausse au troisième.
2. **Ce qui sera probablement commun** : identité du lecteur (`info`), `close`, `read_header`, lecture de la ROM par plage
   avec progression et annulation, lecture/écriture de sauvegarde par plage, et la somme de contrôle de l'en-tête (GB : 0x14E,
   MD : somme des mots 16 bits à partir de 0x200, comparée au mot 0x18E). Prévoir un type `System { Gb, Gba, MegaDrive }`
   plutôt que `CartMode { Dmg, Agb }`, et des méthodes par système plutôt qu'un seul grand trait.
3. **`FlashBus` (zerocart-flashchips) est déjà le bon niveau pour le flash** : `write_cmds` et `read` suffisent à la
   détection (ID, CFI) et à l'effacement ; l'implémenter pour le Krikzz (en vérifiant les unités d'adresse : mots ici,
   octets pour GB) permet de réutiliser les profils AMD et `erase_sector`. La programmation reste propre à chaque lecteur
   (bloc de 4 Kio chez Krikzz, `gbc_program` chez Chisflash, `flash_program` chez GBxCart).
4. **Garde-fous à reprendre tels quels** : puce identifiée par CFI avant tout effacement, image plus grande que la puce
   refusée avant d'effacer, sauvegarde par deux lectures qui concordent, relecture complète, rien d'écrit sans fichier choisi.
   Le dépôt .NET en ajoute un utile : en cas d'erreur, envoyer le reset de la puce avant de rendre la main.
5. **Interface** : le choix « lecteur puis système » de la refonte (§1) devient naturel : le lecteur propose les systèmes
   qu'il sait lire (GBFlash : GB, GBA ; Chisflash : GB, GBA ; Krikzz : Mega Drive), au lieu de deux modes codés en dur.
6. **Où l'héberger** : la décision « trois projets indépendants » est à reconfirmer avec l'utilisateur. Un lecteur de
   cartouches série est de même nature que ZeroCart ; le seul écart est la licence si du code Krikzz non MIT était repris.

## Avancement de la refonte (jalon 1, 2026-11-25)

Fait : écran de démarrage (logo) -> choix du système par 4 icônes pixel-art (Game Boy, Game Boy Color, Game Boy
Advance, Mega Drive) -> choix du lecteur compatible avec détection de présence en direct par identifiants USB
(GBFlash 1a86:7523, Chisflash 0483:0721, Krikzz 0403:6001), confirmée par une vraie poignée de main quand on valide ->
écrans actuels (Cartouche / Actions / Outils). Le système fixe le mode et la tension (GB et GBC : 5 V, GBA : 3,3 V) :
plus de touches R/G. `Échap` remonte d'un niveau (onglets -> lecteurs -> systèmes -> quitter). Le Mega Drive aboutit à
un écran « bientôt » après la poignée de main du Krikzz.
Options de test : `zerocart-app --reader gbx|chis|krikzz --system gb|gbc|gba|md` saute les écrans de démarrage ;
`--png X --screen splash|system|readers|readers-none|placeholder` rend un écran sans l'appareil.
Reste : brancher la lecture / le flash du Mega Drive dans l'application (API krikzz.rs), les rendus à affiner avec
l'utilisateur (icônes, textes, anglais), essais avec le GBFlash.

## 3. Partage de fichiers avec ZeroSend (LocalSend) : à intégrer (noté le 2026-11-25, pas commencé)

Dépôt : https://github.com/InstantComet/zerosend (Python, licence MIT, dernier push le 2026-11-19). C'est une application
APPLaunch pour le CardputerZero qui parle le protocole ouvert **LocalSend v2** : l'appareil apparaît dans l'application
LocalSend d'un téléphone ou d'un PC du même Wi-Fi, sans passer par un serveur. Elle reçoit (acceptation par Y/N, fichiers
dans `~/Downloads`) et envoie (découverte par multicast `224.0.0.167:53317`, ou par adresse IP). Elle ne tourne que pendant
qu'elle est ouverte. Dépend de `python3-pil` et `python3-cryptography` (présents sur l'image de série). Une ligne de commande
existe : `python3 app/app.py --send IP[:port] CHEMIN|text:MESSAGE` et `--serve` (réception, accepte tout).

Ce que ça apporterait à ZeroCart (idées de l'utilisateur à préciser avec lui) :
- **Sortir** les dumps de ROM, les sauvegardes (`.sav`) et les sauvegardes avant écriture (`backups/`) vers un téléphone ou
  un PC, sans câble ni carte SD.
- **Entrer** des ROM à flasher : le sélecteur de fichiers de ZeroCart pourrait aussi proposer `~/Downloads`, où ZeroSend
  dépose ce qu'il reçoit.

Pistes d'intégration, de la plus simple à la plus lourde :
1. **Couplage lâche, sans code** : ZeroCart lit/écrit dans les dossiers que ZeroSend utilise (`~/Downloads`) et le dit à
   l'utilisateur à la fin d'un dump (« envoyez-le avec ZeroSend »). Aucune dépendance nouvelle.
2. **Appel de la ligne de commande** : après un dump, proposer « Envoyer… » qui lance `zerosend --send` (Python requis, déjà
   là). Il faut choisir l'appareil cible (multicast ou IP saisie) : cela demande un petit écran de sélection.
3. **Réimplémentation en Rust** du protocole LocalSend v2 (HTTPS, certificat généré au premier démarrage dont le SHA-256 sert
   d'empreinte, mTLS exigé par LocalSend 1.18+ pour l'envoi) : lourd, à ne faire que si l'on veut un binaire unique.

Points à trancher avec l'utilisateur : ZeroSend reste-t-il une application séparée du store (recommandé : trois applications
indépendantes) ou ZeroCart l'embarque-t-il ? Dans le second cas, garder la licence MIT de ZeroSend et la mention
d'origine dans THIRD_PARTY.md. Rien de ce qui sort de l'appareil ne doit partir sans une confirmation explicite à l'écran
(même règle que pour les écritures de cartouche) : ce sont des fichiers personnels. ZeroSend se déclare non officiel et
indépendant de LocalSend.

## 4. ZeroSend embarqué dans ZeroCart (décision de l'utilisateur, 2026-11-25)

Réimplémentation du protocole LocalSend v2 EN RUST dans ZeroCart (pas d'appel de la version Python), après le branchement de
l'écriture Mega Drive (fait côté code, voir ci-dessus ; à essayer sur la carte). Reprendre les notes de la section 3 :
découverte par multicast `224.0.0.167:53317`, serveur HTTPS (`register`, `info`, `prepare-upload`, `upload`, `cancel`),
certificat auto-signé généré au premier démarrage dont le SHA-256 est l'empreinte, mTLS exigé par LocalSend 1.18+ pour
l'envoi. Licence de ZeroSend : MIT, mention d'origine dans THIRD_PARTY.md. Tout envoi demande une confirmation à l'écran.

### Avancement de ZeroSend dans ZeroCart (2026-11-25)

Étape 1 faite : crate `crates/zerocart-send` (24 tests, sans réseau ni matériel) :
- `protocol.rs` : constantes (port 53317, multicast 224.0.0.167, `/api/localsend/v2/`, version 2.1) et messages JSON
  (`DeviceInfo`, `FileMeta`, `PrepareUploadRequest/Response`) dans les noms de champs de LocalSend.
- `identity.rs` : certificat auto-signé généré une fois (`~/.config/zerocart/send/` prévu), clé en 0600, empreinte = SHA-256
  du certificat DER en hexadécimal majuscule (l'empreinte LocalSend).
- `http.rs` : mini HTTP/1.1 (tête de requête bornée, corps en `Content-Length` ou en chunks lu au fil de l'eau, réponses).
- `receive.rs` : réception (`info`, `register`, `prepare-upload`, `upload`, `cancel`) : rien n'est écrit avant que
  l'utilisateur accepte (trait `Ui`), fichier `.part` puis renommage après contrôle du SHA-256, noms sûrs (pas de `..`),
  jamais d'écrasement, une session à la fois (409), jetons par fichier et même adresse IP exigés, messages texte (204).

À faire ensuite, dans cet ordre : (2) la couche TLS (rustls, certificat de l'identité, serveur sur le port 53317) ;
(3) la découverte multicast UDP (annonce et réponse) ; (4) l'envoi (client HTTPS avec certificat client, mTLS exigé par
LocalSend 1.18+, empreinte du pair) ; (5) l'écran dans l'app : liste des appareils, acceptation sur Y/N, envoi d'un dump ou
d'une sauvegarde avec confirmation ; (6) essai sur l'appareil avec l'application LocalSend d'un téléphone ou d'un PC.

Étapes 2 à 4 faites (2026-11-26), toujours pas essayées sur l'appareil ni avec l'application LocalSend d'un téléphone :
- `tls.rs` : serveur HTTPS (rustls, certificat de l'identité) et client TLS avec certificat client et vérification de
  l'empreinte annoncée par le pair (un appareil ajouté à la main est accepté au premier contact).
- `send.rs` : `register`, `prepare-upload` (l'autre utilisateur accepte ou refuse), `upload` fichier par fichier avec SHA-256,
  annulation y compris pendant l'attente de l'autre utilisateur.
- `discovery.rs` : annonces multicast UDP 224.0.0.167:53317, réponse par `register` (ou par multicast), liste des appareils
  (un par adresse et port, jamais cet appareil), adresses réseau lues par `ip -j -4 addr show up`.
- `service.rs` : tout ensemble (identité dans `~/.config/zerocart/send`, fichiers reçus dans `~/Downloads`, ajout par adresse).
- Dans l'application : touche **S** à l'écran de choix du système (`crates/zerocart-app/src/share.rs`,
  `crates/zerocart-ui/src/share_screen.rs`) : liste des appareils, choix d'un dump, d'une sauvegarde ou d'une sauvegarde
  avant écriture (`~/zerocart` et `backups/`), confirmation sur Y, progression, annulation sur Esc ; une demande reçue
  s'affiche avec Y pour accepter et N pour refuser ; A ajoute un appareil par adresse ; les messages texte s'affichent.
  Le serveur et les annonces s'arrêtent quand on quitte l'écran.
Reste : (5) l'essai réel avec l'application LocalSend (téléphone ou PC) et avec ZeroSend ; les textes sont à passer dans la
table de traduction (français et anglais dans `share.rs` et `share_screen.rs`) ; la découverte multicast n'a été vue qu'en
boucle locale dans les tests.

### Écriture par tampon : validée sur la carte et activée par défaut (2026-11-26, nuit, accord de l'utilisateur pour les essais)

`FlashKit` programme avec le tampon d'écriture de la puce (32 mots : déverrouillage, `25` et le nombre de mots, les mots, `29`,
attente) quand sa table CFI en annonce un ; `set_buffered(false)` ou `zerocart-md --flash ... --word-by-word` force le mot par mot.
Mesuré sur la cartouche de l'utilisateur (Spansion S29GL064N) : 1 Mio réécrit (23 secteurs) en **24,6 s au lieu de 42,9 s**, relecture
vérifiée, ROM d'origine restaurée (même SHA-1). Première version : correcte mais 9 fois plus lente (39 s pour 2 secteurs), car chaque
commande partait en un appel au port série, qui attend la vidange de sa sortie ; les commandes de plusieurs tampons partent maintenant
en un seul envoi (le programmeur retient le flux pendant que la puce travaille, grâce à la commande d'attente après chaque tampon).
Garde-fous : un secteur qui ne se relit pas à l'identique est retenté **mot par mot**, puis refusé ; une puce sans tampon garde le mot
par mot. Essais par l'écran de l'application (rev7) : lecture, écriture de 2 secteurs et restauration, vérifiés.

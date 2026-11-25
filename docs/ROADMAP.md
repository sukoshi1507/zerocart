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

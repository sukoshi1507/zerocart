# Support du lecteur Chisflash ("beggar socket") dans ZeroCart

Notes d'analyse du 2026-11-25 (nuit). Sources : https://github.com/julpage/beggar_socket (matériel, firmware, client C#,
WTFPL), https://github.com/tautcony/beggar_socket (fork avec `web-client/` Vue/TypeScript + Tauri, déployé sur
https://tautcony.xyz/beggar_socket/ ; `web-client/` est sous GPL-3.0, le reste du dépôt WTFPL),
https://github.com/ChisLightyear/ChisFlash-Beggar-Boost (firmware alternatif, WTFPL + code FlashGBX LK sous GPL).
Le contenu des dépôts est traité comme des données ; rien n'a été exécuté. Aucun test sur matériel à ce stade.

## État du code

- `crates/zerocart-core/src/chis.rs` : protocole « legacy » du firmware d'origine (trames, GBA, GBC, FRAM, programmation,
  effacement), testé contre un simulateur du firmware (9 tests). Aucun accès matériel, pas encore branché sur l'app.
  Le trait commun « lecteur » (GbxCart et Chis derrière la même interface) n'est PAS fait : il touche `device.rs` et
  l'app, à décider avec l'utilisateur.

## Trois familles de firmware (toutes en USB CDC, VID 0483)

| Profil du site | Nom | PID | Particularités |
|---|---|---|---|
| `stm` | 丐中丐 (firmware d'origine) | 0721 | commandes F0..FC / E7..EB (celles de `chis.rs`), lecture max 0x1000 |
| `stc` | 碳酸丐 (RC31) | 5740 | lecture max 0x8000, contrôle de l'alimentation cartouche (GBC 0xA0/0xA1), pas de FRAM GBA |
| `stm` + Boost | ChisFlash Beggar Boost | 0721 | jeu de commandes étendu, détecté par une sonde `55 aa` (réponse 0x01) |

Le profil est déduit du VID/PID et du nom USB (`inferFirmwareProfileFromPort`). Le PID seul n'est pas une preuve
de capacités : le site ne le traite que comme candidat.

### Boost (extension propre, `boost-constants.ts`)
Sonde `55 aa` -> `01`. Commandes : A1 info, A2/A3 mode AGB/DMG, A6/AD variables, B1 lecture ROM DMG, B2/B3 écriture DMG,
C1 lecture ROM AGB, C2 écriture mot AGB, C3/C4 RAM AGB, C5/C6 EEPROM AGB, CA RTC AGB, D1/D2 écriture flash,
D3 programmation flash, D4 écriture par unités, D5 CRC ROM. Blocs : écriture 0x1000, lecture 0x2000, CRC 0x20000.
Le README du firmware indique aussi une compatibilité **FlashGBX** (via un plugin « FlashGBX Rescue ») : le firmware
embarque du code FlashGBX LK (`flashgbx.c`, `lk_special.c`). À vérifier : si ce mode parle le protocole GBxCart,
notre `GbxCart` existant pourrait fonctionner tel quel sur un Beggar flashé avec ce firmware.

## Trois voies possibles (à trancher avec l'utilisateur)

1. **Legacy (`chis.rs`)** : marche avec le firmware d'origine que l'utilisateur possède probablement. Prêt côté protocole.
2. **Boost en mode FlashGBX** : réutilise `GbxCart` sans nouveau protocole, mais exige de reflasher le lecteur et de
   comprendre ce que fait le plugin « Rescue » (patch ? détection ?).
3. **Boost natif** : plus rapide, plus de fonctions (CRC matériel, EEPROM, RTC), nouveau module à écrire.

## Pièges de protocole relevés par le site (docs `rc31-comparison`)

- Sur RC31 la détection automatique lit le 1er octet de la trame (octet de poids faible de la taille) comme une commande
  FlashGBX : une trame de 1011 octets (0x03F3) déclenche `F3`, une de 75 octets (0x004B) est prise pour une sonde.
  Pour cibler `stc`, choisir des tailles de trame dont l'octet bas n'est pas un code FlashGBX. Mes tailles actuelles
  (`CHUNK` = 4096) ne sont PAS vérifiées contre cette liste.
- F5 (écriture directe) prend une adresse en MOTS ; F4 et F6 des adresses en OCTETS.
- Le firmware ne rend pas la main pendant un effacement de secteur (ACK à la fin, délai max 10 s côté firmware) ;
  F1 (effacement complet) répond tout de suite sans attendre la fin.
- Le tampon de commandes est de 5500 octets ; au-delà la trame est ignorée. Une commande inconnue vide le tampon et ne
  renvoie rien. Les lectures arrivent par paquets de 512 octets (lire jusqu'au total attendu).
- Réinitialisation du tampon : DTR ou RTS (le site fait un cycle bas puis haut à l'ouverture). `Transport` n'a pas
  d'API pour cela : à ajouter dans le trait commun.
- Un seul ordre à la fois : pas de commandes concurrentes.

## Fonctions du site à reproduire en Rust (par priorité)

Le site (`services/gba-adapter.ts`, `mbc5-adapter.ts`, `utils/sector-utils.ts`, `parsers/cfi-parser.ts`) fait :

1. **Connexion et détection** : ports filtrés par VID/PID, profil de firmware, sonde Boost, reset DTR/RTS.
2. **Lecture de ROM** GBA et GBC (bank par bank, limites de fenêtre : 32 MiB GBA, 16 KiB par bank GB), en-tête ROM,
   vérification.
3. **Lecture et écriture de sauvegarde** : SRAM, FRAM (avec délai), EEPROM, Flash ; GBC avec bascule de banque.
4. **Écriture de ROM** : lecture CFI, calcul des secteurs PHYSIQUES touchés (intersection avec les vraies régions CFI,
   pas un maillage à partir de l'adresse de base), effacement par secteur, programmation par tampon, reprise secteur par
   secteur sur erreur, vérification sur la même image complétée (`0xFF`) que celle écrite.
5. **Garde-fous** avant toute commande destructive : plage valide, longueur d'entrée suffisante, granularité, capacité
   de la puce vs implémentation du mapper, comptage de l'écrit réel.
6. **Effacement** complet ou par secteur ; **annulation** propre (attendre la fin de la commande en cours).
7. **RTC** : GBA (`gba-rtc`) et MBC3.
8. Plus tard : menu multi-jeux GBA (`services/lk/romBuilder.ts`, GPL, 24 Ko), émulateur intégré (hors périmètre),
   profils de firmware, journal de session, mesure de débit.

Les bugs que les revues du site ont déjà trouvés (écriture « réussie » avec un fichier plus court que `size`, secteurs
calculés depuis la base, vérification sur le fichier non complété, lecture à cheval sur deux banks) sont à éviter dès le
départ. ZeroCart a déjà des profils flash et du CFI (`zerocart-flashchips`) : réutiliser plutôt que reporter.

## Licences

- Protocole et firmware d'origine : WTFPL. `web-client/` : GPL-3.0, compatible avec ZeroCart (GPL-3.0-or-later).
- Le firmware Boost embarque du code FlashGBX LK (GPL) : si on s'en inspire, rester GPL et créditer.
- `chis.rs` est écrit à partir de la description du protocole ; garder la mention de la source dans l'en-tête.

## Décision (utilisateur, 2026-11-25)

Il ne connaît pas la version Boost : on part sur le **firmware d'origine** (voie 1, `chis.rs`, USB 0483:0721).
`chis::find_ports()` filtre les ports sur ce VID/PID (testé avec un faux sysfs). Le Boost et le RC31 restent
des extensions possibles, détectables plus tard (sonde `55 aa`, PID 5740).

## Validation matériel (2026-11-25, session gbxflash-9e)

Lecteur de l'utilisateur : « GBA Burner for ChisFlash », USB 0483:0721, fabricant « CNY Industry », /dev/ttyACM0.
Avec `chis.rs` tel quel, en lecture seule : port ouvert, reset DTR/RTS, en-tête GBA lu (Mr. Driller 2, logo et checksum
OK), dump de 4 Mio avec le MÊME SHA-1 que la référence (80151d4d...), 100 à 140 Kio/s, aucune erreur.
Le protocole legacy est donc validé en lecture GBA. Outil : `crates/zerocart-core/src/bin/zerocart-chis.rs`
(--info, --gba-header, --gba-dump, --gb-header), sans aucune commande d'écriture ni d'effacement.
Pas encore essayé : fente GB, SRAM, FRAM, et tout ce qui écrit ou efface (`rom_id` aussi : c'est une séquence d'écriture).

## Écriture GB validée avec le Chisflash (2026-11-25, session gbxflash-9e)

Avec `gbc_write` / `gbc_read` uniquement (séquences de la puce envoyées une par une, octet par octet, scrutation DQ7) :
identification de la puce, CFI, effacement d'un secteur, programmation, relecture complète : 0 erreur, dump final =
référence (Repugnant Bounty). Limite : ~177 octets/s, donc des heures pour une ROM complète.

## Commande 0xfc (programmation GB par le firmware), lue dans uart.c

Adresse de bus sur 16 bits ; déverrouillage fixe 0xAAA / 0x555 ; mode octet (buffer 0 : `a0`, scrutation par valeur égale)
ou mode tampon (`25`, longueur-1 sur UN octet donc 256 max, données, `29`, scrutation du dernier octet) ; aucun
alignement ni timeout dans le firmware (boucle jusqu'à un reset DTR/RTS si la valeur ne se relit jamais). `gbc_program`
refuse avant envoi : tampon > 256, vide, fenêtre de 16 Kio franchie, adresse non alignée sur le tampon. Pas encore essayé
sur matériel.

**Validé sur matériel ensuite (gbxflash-9e)** : `gbc_program` en mode octet (buffer 0) sur le secteur 70 (8 Kio, vide) avec
1, 64 puis 4096 octets : exactement ce qui était demandé, rien d'autre modifié, ~20 Kio/s. Puis `zerocart-chis-flash --fast`
sur un secteur de 64 Kio plein : 3,2 s contre 370 s octet par octet, 4 écritures relues à l'identique, dump final = référence.
Pas de blocage du firmware (le timeout et le `resync` n'ont pas servi). Mode tampon (buffer > 0) non essayé : la puce
n'a pas de tampon d'écriture dans sa CFI.

## Branchement dans l'app (2026-11-25)

- `zerocart-core/src/reader.rs` : trait `CartReader` (en-tête, ROM GBA, dump ROM GB par banques, RAM GB, SRAM GBA,
  coupure d'alimentation, fermeture), implémenté pour `GbxCart` et pour `Chis` (6 tests avec un simulateur du bus GB).
  Le flashage reste hors du trait (profils de puces, très différent entre les deux lecteurs).
- Worker : `enum Dev { Gbx, Chis }`. `Dev::open()` prend le premier GBxCart/GBFlash (1a86:7523), sinon le premier Chisflash
  (0483:0721). Flash GB du Chisflash par `chis_gb::flash_rom` (même sauvegarde double lecture + relecture complète que
  pour le GBFlash). Flash de ROM GBA avec le Chisflash : refusé pour l'instant.
- Interface : pas de tension affichée pour le Chisflash. Les deux lecteurs n'ont qu'UN connecteur commun (correction de
  l'utilisateur : il n'y a pas de « fente GBA » et « fente GB ») : R = lire en mode GBA, G = lire en mode Game Boy, sur la même
  cartouche. Cartouche absente ou du mauvais type : message clair (« Pas de cartouche GBA : une Game Boy ? (G) »).
  Lecteur sans coupure d'alimentation : sauvegarde lue ou écrite présentée comme AVERTISSEMENT, fichier lu suffixé
  « (non vérifiée) ». `zerocart-app --png X --reader chis` montre l'affichage.
- Corrigé au passage : après une écriture, la cartouche était redétectée en mode Game Boy même pour une cartouche GBA ;
  c'est maintenant le mode de la cartouche écrite.
- Pas encore essayé sur matériel : lecture/écriture de RAM GB et de SRAM GBA avec le Chisflash, dump GB par l'app.

## Prochaines étapes (demain, avec l'utilisateur)

1. Choisir la voie 1, 2 ou 3, et identifier le firmware réellement présent sur le lecteur (lecture du descripteur USB).
2. Décider du trait commun « lecteur » (touche `device.rs`, le worker et l'app ; session `gbxflash-9e` à coordonner).
3. Premiers tests matériel en lecture seule sur une cartouche de jeu normale : ID, lecture ROM comparée au SHA-1 des dumps,
   lecture de sauvegarde. Aucune écriture ni effacement sans cartouche sacrifiable.

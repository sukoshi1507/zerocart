# Cahier des Charges & Architecture : FlashGBX-Rust-LVGL pour M5Stack Cardputer Zero

## 1. Contexte et Objectif du Projet
L'objectif est de concevoir et développer une implémentation native en **Rust** avec une interface graphique **LVGL** inspirée de **FlashGBX** (`Lesserkuma/FlashGBX`), optimisée pour fonctionner sur le **M5Stack Cardputer Zero**.

### Pourquoi Rust + LVGL ?
- **Empreinte mémoire minimale** : Le Cardputer Zero dispose de 512 Mo de RAM (Linux ARM64 sur CM0). Remplacer la pile Python + PyQt par un binaire statique/dynamique Rust + LVGL permet de réduire l'usage RAM à moins de 15-20 Mo et d'éliminer les dépendances lourdes (X11/Wayland, interpréteur Python).
- **Performance et réactivité** : Gestion asynchrone/multi-thread native des transferts série USB et rendu fluide sur le framebuffer Linux (`/dev/fb0`).

---

## 2. Spécifications Matérielles Cibles (M5Stack Cardputer Zero)
- **SoC / Architecture** : Raspberry Pi CM0 (ARM64 / `aarch64-unknown-linux-gnu`), 512 Mo RAM.
- **Écran** : LCD couleur **320 × 170 pixels** exposé via le framebuffer Linux (`/dev/fb0`) ou DRM.
- **Entrées** : Clavier physique intégré exposé via Linux evdev (`/dev/input/eventX`). Pas d'écran tactile ni de souris.
- **Connectivité USB** : Port USB-A Host / USB-C Host pour connecter les lecteurs de cartouches externes.
- **Matériel supporté (Lecteurs)** :
  - Priorité 1 : **GBxCart RW** (v1.4 / v1.4a / v1.4c) et **GBFlash** (v1.2 / v1.3) — protocole série CDC/CH340.
  - Priorité 2 : **Joey Jr** (V2++) et **Game Bub**.

---

## 3. Architecture Logicielle Cible

L'application doit être découpée en un workspace Cargo modulaire afin de séparer strictement la logique bas niveau (protocole et cartouches) de l'interface LVGL :

```text
flashgbx-rs/
├── Cargo.toml                  # Workspace Cargo
├── crates/
│   ├── flashgbx-core/          # Protocole série, détection matériel, parsing headers ROM (DMG/CGB/AGB)
│   ├── flashgbx-mappers/       # Gestion des mappers GB (MBC1/2/3/5/30, Camera...) et types de sauvegarde GBA (SRAM, EEPROM, Flash 512K/1M)
│   ├── flashgbx-flashchips/    # Commandes CFI et tables de flashage pour cartouches réinscriptibles et bootlegs
│   └── flashgbx-lvgl-ui/       # Binaire principal : boucle LVGL, driver fbdev + evdev, UI 320x170
└── configs/                    # Fichiers de définition JSON/TOML des puces Flash (compatibles/convertis depuis FlashGBX)
```

---

## 4. Spécifications Techniques Détaillées

### 4.1. Couche Communication & Protocole (`flashgbx-core`)
- **Port série** : Utiliser la crate `serialport` (ou `tokio-serial`) pour communiquer avec les périphériques `/dev/ttyUSB*` et `/dev/ttyACM*` (VID/PID : `1a86:7523`, `0483:5740`, `1209:b010`, `16c0:27dd`).
- **Portage du protocole Lesserkuma** :
  - Implémenter la séquence de handshake et d'identification du firmware (GBxCart RW / GBFlash).
  - Implémenter le contrôle de tension cartouche (3.3V pour GBA, 5V pour GB/GBC) et la mise sous/hors tension automatique sécurisée.
  - Implémenter les commandes de lecture/écriture par blocs (ROM et RAM) avec vérification CRC/checksum.
- **Parsing d'en-têtes (Header Parsing)** :
  - **Game Boy / Color (`0x0100` - `0x014F`)** : Titre, code CGB, type de cartouche (mapper), taille ROM, taille RAM, checksums d'en-tête et global.
  - **Game Boy Advance (`0x000` - `0x0BF`)** : Titre du jeu (12 octets), Game Code (4 octets), Maker Code, détection heuristique du type de sauvegarde (`EEPROM_V`, `SRAM_V`, `FLASH_V`, `FLASH512_V`, `FLASH1M_V`) et détection de la taille de la ROM.

### 4.2. Gestion des Mappers et Sauvegardes (`flashgbx-mappers`)
- **Game Boy / GBC** :
  - Support complet de la pagination mémoire (bank switching) en lecture et écriture pour : `ROM ONLY`, `MBC1`, `MBC2`, `MBC3` (avec support registres RTC), `MBC30`, `MBC5`, `MBC7`, `MAC-GBD` (Game Boy Camera).
- **Game Boy Advance** :
  - Lecture/écriture directe de la ROM (jusqu'à 32 Mo, par streaming sur disque pour éviter de charger 32 Mo en RAM).
  - Protocoles de lecture/écriture pour : `SRAM / FRAM` (32 Ko / 64 Ko), `EEPROM` (4 Kbit / 64 Kbit, bus série), `Flash` (64 Ko / 128 Ko avec gestion des commandes JEDEC/bank switching).

### 4.3. Support des Cartouches Flash & Reproduction (`flashgbx-flashchips`)
- Implémenter la requête **CFI (Common Flash Interface)** et l'identification JEDEC (Manufacturer ID / Device ID).
- Charger dynamiquement les profils de cartouches flash depuis des fichiers légers (TOML/JSON) dérivés des dossiers `config/` du dépôt Python original `FlashGBX`.
- Supporter l'effacement de secteurs/puce et l'écriture avec vérification immédiate.

### 4.4. Interface Utilisateur LVGL (`flashgbx-lvgl-ui`)
- **Bindings Rust** : Utiliser `lvgl` (ou bindings `bindgen` directs sur LVGL v9.x compilé via `cc` dans `build.rs`).
- **Drivers Linux natifs (sans X11/Wayland)** :
  - Affichage : Driver `LV_USE_LINUX_FBDEV` pointant sur `/dev/fb0` (résolution 320×170, format couleur RGB565).
  - Clavier : Driver `LV_USE_EVDEV` pointant sur `/dev/input/eventX`.
- **Navigation 100% Clavier (`lv_group_t`)** :
  - Tous les widgets interactifs doivent être enregistrés dans un `lv_group` par défaut.
  - Touches : Flèches directionnelles / `Tab` pour naviguer, `Entrée` pour valider, `Échap` / `Backspace` pour retour/annuler.
- **Architecture de l'UI (Écran 320×170)** :
  - **Barre de statut haute (22 px)** : Nom du lecteur connecté + version firmware + Voltage actif (`5V` / `3.3V`).
  - **Écran 1 : Infos Cartouche** : Titre du jeu, Game Code, Mapper / Type de sauvegarde détecté, taille ROM/Save, état CRC (`OK` / `Invalid`).
  - **Écran 2 : Actions & Transferts** : Menu compact avec 4 actions (`Lire ROM`, `Lire Save`, `Écrire Save`, `Flasher ROM`), explorateur de fichiers minimaliste (pour choisir un `.sav` ou `.gb/.gbc/.gba` sur la carte SD) et barre de progression (`lv_bar`) avec débit (Ko/s) et temps restant.
  - **Écran 3 : Outils / Flash Cart** : Bouton `Analyser Puce Flash (CFI)` et sélection manuelle du type de cartouche bootleg si l'auto-détection échoue.

---

## 5. Contraintes d'Ingénierie et Bonnes Pratiques
1. **Zéro allocation massive en RAM** : Les opérations de dump et de flashage de ROM (jusqu'à 32 Mo pour la GBA) doivent utiliser des buffers circulaires ou par blocs (ex: 4 Ko à 64 Ko) lus/écrits en streaming via `std::io::BufReader` / `BufWriter`.
2. **Thread-Safety LVGL** : LVGL n'est pas thread-safe. Exécuter la boucle `lv_timer_handler()` dans le thread principal et exécuter les opérations USB/série bloquantes dans un `std::thread` dédié communiquant via des canaux (`std::sync::mpsc`) pour mettre à jour la barre de progression.
3. **Cross-compilation** : Fournir un fichier `.cargo/config.toml` et un `Dockerfile` ou configuration `cross` ciblant `aarch64-unknown-linux-gnu` pour compiler rapidement depuis un PC x86_64.

---

## 6. Plan d'Implémentation par Étapes (Roadmap pour le LLM)
- **Étape 1 (Socle & Protocole)** : Initialiser le workspace Cargo, implémenter la détection série et le handshake GBxCart RW dans `flashgbx-core` avec un binaire CLI de test (`--info`, `--dump-rom`).
- **Étape 2 (Mappers & Saves)** : Ajouter la gestion complète des mappers GB (`MBC1`, `MBC3`, `MBC5`) et des sauvegardes GBA (`SRAM`, `EEPROM`, `FLASH`) dans `flashgbx-mappers`.
- **Étape 3 (Intégration LVGL)** : Configurer la compilation de LVGL v9 en C via `build.rs` dans `flashgbx-lvgl-ui`, activer `fbdev` + `evdev`, et valider l'affichage d'un écran de test 320×170 contrôlable au clavier.
- **Étape 4 (Assemblage UI <-> Core)** : Connecter l'interface LVGL aux threads de travail de `flashgbx-core`, intégrer le sélecteur de fichiers et la barre de progression.
- **Étape 5 (Support Flashcarts)** : Implémenter `flashgbx-flashchips` et convertir les définitions de puces Flash depuis le dépôt `Lesserkuma/FlashGBX`.

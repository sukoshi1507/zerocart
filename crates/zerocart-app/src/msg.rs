// SPDX-License-Identifier: GPL-3.0-or-later
//! The messages of the worker thread (what it reports about a reader, a cartridge, a transfer), in the four
//! languages. A message that starts with '!' is shown as a warning: the work was done but could not be verified.
use zerocart_ui::{loc, Lang};

/// The kilobyte unit: French keeps Ko, the other languages use KiB.
pub fn ko(l: Lang) -> &'static str {
    loc!(l, fr: "Ko", en: "KiB", ja: "KiB", zh: "KiB")
}

pub fn cancelled(l: Lang) -> String {
    loc!(l, fr: "annulé", en: "cancelled", ja: "中止しました", zh: "已取消").into()
}

pub fn batch_gba_inside(l: Lang) -> String {
    loc!(l,
        fr: "Cartouche GBA détectée : le mode série est réglé sur Game Boy".into(),
        en: "A GBA cartridge is in: the serial mode is set to Game Boy".into(),
        ja: "GBAカートが入っています：連続書込はGame Boy設定です".into(),
        zh: "检测到GBA卡带：连续烧录设为Game Boy".into())
}

pub fn no_cartridge(l: Lang) -> String {
    loc!(l, fr: "aucune cartouche", en: "no cartridge", ja: "カートがありません", zh: "没有卡带").into()
}
pub fn not_found(l: Lang, what: &str) -> String {
    loc!(l,
        fr: format!("{what} : introuvable"),
        en: format!("{what} not found"),
        ja: format!("{what}が見つかりません"),
        zh: format!("未找到{what}"))
}
pub fn no_answer(l: Lang, e: &str) -> String {
    loc!(l,
        fr: format!("le lecteur ne répond pas : {e}"),
        en: format!("the reader does not answer: {e}"),
        ja: format!("リーダーが応答しません：{e}"),
        zh: format!("读卡器没有响应：{e}"))
}
pub fn soon(l: Lang, what: &str) -> String {
    loc!(l, fr: format!("{what} : bientôt"), en: format!("{what}: coming soon"), ja: format!("{what}：近日対応"), zh: format!("{what}：即将支持"))
}
pub fn md_board_no_answer(l: Lang, e: &str) -> String {
    loc!(l,
        fr: format!("la carte ne répond pas : {e}"),
        en: format!("the board does not answer: {e}"),
        ja: format!("ボードが応答しません：{e}"),
        zh: format!("开发板没有响应：{e}"))
}

// ---- shared by the readers ----
pub fn mapper_unsupported(l: Lang) -> String {
    loc!(l, fr: "mapper pas encore géré", en: "mapper not supported yet", ja: "このマッパーは未対応です", zh: "暂不支持该映射器").into()
}
pub fn no_save_on_cart(l: Lang) -> String {
    loc!(l, fr: "cette cartouche n'a pas de sauvegarde", en: "this cartridge has no save", ja: "このカートにはセーブがありません", zh: "该卡带没有存档").into()
}
pub fn game_not_in_database(l: Lang) -> String {
    loc!(l,
        fr: "jeu absent de la base : type de sauvegarde inconnu",
        en: "game not in the database: unknown save type",
        ja: "ゲームがデータベースにありません：セーブの種類が不明です",
        zh: "游戏不在数据库中：存档类型未知")
    .into()
}
pub fn save_type_unsupported(l: Lang, name: &str) -> String {
    loc!(l,
        fr: format!("sauvegarde « {name} » : pas encore gérée"),
        en: format!("save type \"{name}\": not supported yet"),
        ja: format!("セーブ形式「{name}」は未対応です"),
        zh: format!("暂不支持存档类型“{name}”"))
}
pub fn rom_exceeds_chip(l: Lang, rom: &str, chip: &str) -> String {
    loc!(l,
        fr: format!("la ROM ({rom}) dépasse la puce ({chip})"),
        en: format!("the ROM ({rom}) is larger than the chip ({chip})"),
        ja: format!("ROM（{rom}）がチップ（{chip}）より大きいです"),
        zh: format!("ROM（{rom}）超出芯片容量（{chip}）"))
}
pub fn backup_cart_progress(l: Lang) -> String {
    loc!(l,
        fr: "Sauvegarde de la cartouche avant écriture",
        en: "Backing up the cartridge before writing",
        ja: "書き込み前にカートをバックアップ中",
        zh: "写入前正在备份卡带")
    .into()
}
pub fn writing_sectors(l: Lang, done: usize, total: usize, msg: &str) -> String {
    loc!(l,
        fr: format!("Écriture ({done}/{total}) ne pas débrancher : {msg}"),
        en: format!("Writing ({done}/{total}) do not unplug: {msg}"),
        ja: format!("書き込み中（{done}/{total}）抜かないでください：{msg}"),
        zh: format!("正在写入（{done}/{total}）请勿拔出：{msg}"))
}
/// What to say about the backup of the old ROM: its file, or that none was made (the user chose to go without).
fn old_rom_tail(l: Lang, name: Option<&str>) -> String {
    match name {
        Some(n) => loc!(l, fr: format!("ancienne ROM : backups/{n}"), en: format!("old ROM: backups/{n}"), ja: format!("旧ROM：backups/{n}"), zh: format!("旧ROM：backups/{n}")),
        None => loc!(l, fr: "pas de sauvegarde".to_string(), en: "no backup was made".to_string(), ja: "バックアップなし".to_string(), zh: "未备份".to_string()),
    }
}

/// The same for a whole chip (GBA).
fn old_chip_tail(l: Lang, name: Option<&str>) -> String {
    match name {
        Some(n) => loc!(l, fr: format!("ancienne puce : backups/{n}"), en: format!("old chip contents: backups/{n}"), ja: format!("旧チップ内容：backups/{n}"), zh: format!("旧芯片内容：backups/{n}")),
        None => old_rom_tail(l, None),
    }
}

pub fn old_rom_in(l: Lang, e: &str, name: Option<&str>) -> String {
    format!("{e} ; {}", old_rom_tail(l, name))
}
pub fn written_mismatch(l: Lang, name: Option<&str>) -> String {
    let head = loc!(l,
        fr: "ROM écrite mais la relecture diffère",
        en: "ROM written but the read-back differs",
        ja: "ROMを書き込みましたが、読み戻しが一致しません",
        zh: "ROM已写入，但回读结果不一致");
    format!("{head} ; {}", old_rom_tail(l, name))
}
/// The read-back of a Mega Drive flash differs: how many bytes, where, what was expected, and whether a second read
/// gives the same bytes (a stable difference is a bad write, an unstable one a bad contact).
pub fn written_diff_detail(l: Lang, count: usize, first: usize, want: u8, got: u8, stable: bool) -> String {
    let verdict = if stable {
        loc!(l, fr: "2e lecture identique : écriture", en: "2nd read same: bad write", ja: "再読込も同じ：書込不良", zh: "再读相同：写入有误")
    } else {
        loc!(l, fr: "2e lecture différente : contact ?", en: "2nd read differs: contact?", ja: "再読込は異なる：接触？", zh: "再读不同：接触？")
    };
    loc!(l,
        fr: format!("Vérif : {count} octet(s) faux, 1er en 0x{first:X} (attendu {want:02X}, lu {got:02X}) ; {verdict}"),
        en: format!("Verify: {count} byte(s) wrong, first at 0x{first:X} (want {want:02X}, got {got:02X}); {verdict}"),
        ja: format!("検証：{count}バイト不一致、先頭 0x{first:X}（期待 {want:02X}、読出 {got:02X}）；{verdict}"),
        zh: format!("验证：{count}字节不同，首个在0x{first:X}（应为{want:02X}，读到{got:02X}）；{verdict}"))
}
pub fn written_reread_ok(l: Lang, first: usize, count: usize) -> String {
    loc!(l,
        fr: format!("ROM écrite et vérifiée (la 1re relecture avait {count} octet(s) faux dès 0x{first:X} : contact à surveiller)"),
        en: format!("ROM written and verified (the 1st read-back had {count} wrong byte(s) from 0x{first:X}: check the contact)"),
        ja: format!("ROMを書き込み検証しました（最初の読戻しで0x{first:X}から{count}バイト誤り：接触を確認）"),
        zh: format!("ROM已写入并验证（首次回读自0x{first:X}起有{count}字节错误：请检查接触）"))
}
pub fn batch_no_chip(l: Lang) -> String {
    loc!(l,
        fr: "Aucune puce flash ne répond : cartouche mal enfoncée ou non flashable ? (Entrée : réessayer)",
        en: "No flash chip answers: cartridge not pushed in, or not a flash one? (Enter: try again)",
        ja: "フラッシュチップが応答しません：挿入不良か非対応？（Enter：再試行）",
        zh: "没有闪存芯片响应：未插到位或非闪存卡带？（Enter：重试）").into()
}
pub fn verifying(l: Lang) -> String {
    loc!(l, fr: "Vérification de l'écriture", en: "Verifying the write", ja: "書き込みを検証中", zh: "正在验证写入").into()
}
pub fn reading_rom(l: Lang, title: &str) -> String {
    loc!(l, fr: format!("Lecture ROM : {title}"), en: format!("Reading ROM: {title}"), ja: format!("ROM読み出し：{title}"), zh: format!("读取ROM：{title}"))
}
pub fn reading_rom_pass(l: Lang, title: &str, pass: usize) -> String {
    loc!(l,
        fr: format!("Lecture ROM : {title} ({pass}e passe)"),
        en: format!("Reading ROM: {title} (pass {pass})"),
        ja: format!("ROM読み出し：{title}（{pass}回目）"),
        zh: format!("读取ROM：{title}（第{pass}遍）"))
}
pub fn reading_save_progress(l: Lang) -> String {
    loc!(l, fr: "Lecture de la sauvegarde", en: "Reading the save", ja: "セーブを読み出し中", zh: "正在读取存档").into()
}
pub fn unverified_save(l: Lang, file: &str) -> String {
    loc!(l,
        fr: format!("!Attention : sauvegarde NON vérifiée, ce lecteur ne coupe pas l'alimentation (pile morte invisible) : {file}"),
        en: format!("!Warning: save NOT verified, this reader cannot cut the power (a dead battery would go unnoticed): {file}"),
        ja: format!("!注意：セーブは未検証です。このリーダーは電源を切れません（電池切れを検出できません）：{file}"),
        zh: format!("!注意：存档未验证，此读卡器无法断电（无法发现电池耗尽）：{file}"))
}

// ---- Mega Drive ----
pub fn md_save_write_unsupported(l: Lang) -> String {
    loc!(l,
        fr: "Écriture de sauvegarde Mega Drive : pas encore gérée",
        en: "Mega Drive save writing: not supported yet",
        ja: "メガドライブのセーブ書き込みは未対応です",
        zh: "暂不支持写入Mega Drive存档")
    .into()
}
pub fn md_no_sram(l: Lang) -> String {
    loc!(l,
        fr: "Aucune mémoire de sauvegarde ne répond sur cette cartouche (cartouche flash sans SRAM ?) : rien n'est écrit",
        en: "No save RAM answers on this cartridge (a flash cartridge without SRAM?): nothing is written",
        ja: "このカートにはセーブRAMの応答がありません（SRAMのないフラッシュカート？）：何も書き込みません",
        zh: "此卡带没有存档RAM响应（无SRAM的闪存卡带？）：未写入任何内容")
    .into()
}
pub fn md_bad_size(l: Lang) -> String {
    loc!(l,
        fr: "taille de fichier invalide pour une ROM Mega Drive",
        en: "invalid file size for a Mega Drive ROM",
        ja: "メガドライブROMとして不正なファイルサイズです",
        zh: "文件大小不符合Mega Drive ROM")
    .into()
}
pub fn md_not_a_rom(l: Lang) -> String {
    loc!(l,
        fr: "ce fichier n'est pas une ROM Mega Drive valide (en-tête SEGA absent ; les fichiers .smd entrelacés ne sont pas gérés)",
        en: "this file is not a valid Mega Drive ROM (no SEGA header; interleaved .smd files are not supported)",
        ja: "有効なメガドライブROMではありません（SEGAヘッダーなし。インターリーブ形式の.smdは非対応）",
        zh: "该文件不是有效的Mega Drive ROM（没有SEGA文件头；不支持交错格式的.smd）")
    .into()
}
pub fn chip_not_recognised_md(l: Lang) -> String {
    loc!(l,
        fr: "puce flash non reconnue (cartouche non réinscriptible) : rien n'est écrit",
        en: "flash chip not recognised (cartridge cannot be rewritten): nothing written",
        ja: "フラッシュチップを認識できません（書き換え不可のカート）：何も書き込みません",
        zh: "无法识别闪存芯片（卡带不可改写）：未写入任何内容")
    .into()
}
pub fn md_only_amd(l: Lang) -> String {
    loc!(l,
        fr: "seules les puces AMD/Spansion sont gérées : rien n'est écrit",
        en: "only AMD/Spansion chips are supported: nothing written",
        ja: "AMD/Spansionチップのみ対応：何も書き込みません",
        zh: "仅支持AMD/Spansion芯片：未写入任何内容")
    .into()
}
pub fn md_testing_upper(l: Lang) -> String {
    loc!(l,
        fr: "Test de la moitié haute de la puce",
        en: "Testing the upper half of the chip",
        ja: "チップ上半分をテスト中",
        zh: "正在测试芯片上半部分")
    .into()
}
pub fn md_limited_4mb(l: Lang, size: &str) -> String {
    loc!(l,
        fr: format!("Cartouche limitée à 4 Mo : la moitié haute de la puce est inaccessible. ROM de {size} refusée, rien n'est effacé"),
        en: format!("Cartridge limited to 4 MiB: the upper half of the chip is unreachable. {size} ROM refused, nothing erased"),
        ja: format!("このカートは4 MiBまで：チップ上半分にアクセスできません。{size}のROMは拒否しました（何も消去していません）"),
        zh: format!("卡带仅限4 MiB：芯片上半部分无法访问。已拒绝{size}的ROM（未擦除任何内容）"))
}
pub fn reads_differ(l: Lang) -> String {
    loc!(l,
        fr: "deux lectures de la cartouche diffèrent (mauvais contact ?) : rien n'est écrit",
        en: "two reads of the cartridge differ (bad contact?): nothing written",
        ja: "カートの2回の読み出し結果が異なります（接触不良？）：何も書き込みません",
        zh: "两次读取卡带的结果不同（接触不良？）：未写入任何内容")
    .into()
}
pub fn md_written_ok(l: Lang, rewritten: usize, unchanged: usize, name: Option<&str>) -> String {
    let head = loc!(l,
        fr: format!("ROM écrite et vérifiée ({rewritten} secteurs réécrits, {unchanged} inchangés)"),
        en: format!("ROM written and verified ({rewritten} sectors rewritten, {unchanged} unchanged)"),
        ja: format!("ROMを書き込み、検証しました（{rewritten}セクター書き換え、{unchanged}セクター変更なし）"),
        zh: format!("ROM已写入并验证（重写{rewritten}个扇区，{unchanged}个未变）"));
    format!("{head} ; {}", old_rom_tail(l, name))
}
pub fn no_md_cartridge(l: Lang) -> String {
    loc!(l, fr: "aucune cartouche Mega Drive", en: "no Mega Drive cartridge", ja: "メガドライブのカートがありません", zh: "没有Mega Drive卡带").into()
}
pub fn md_unknown_size(l: Lang) -> String {
    loc!(l,
        fr: "taille de ROM inconnue (en-tête invalide)",
        en: "unknown ROM size (invalid header)",
        ja: "ROMサイズが不明です（ヘッダーが無効）",
        zh: "ROM大小未知（文件头无效）")
    .into()
}
pub fn md_rom_read_ok(l: Lang, file: &str, crc: u32) -> String {
    loc!(l,
        fr: format!("ROM lue, checksum de l'en-tête OK (CRC32 {crc:08x}) : {file}"),
        en: format!("ROM read, header checksum OK (CRC32 {crc:08x}): {file}"),
        ja: format!("ROMを読み出しました。ヘッダーのチェックサムOK（CRC32 {crc:08x}）：{file}"),
        zh: format!("ROM已读取，文件头校验和正确（CRC32 {crc:08x}）：{file}"))
}
pub fn md_rom_read_checksum_differs(l: Lang, file: &str, crc: u32) -> String {
    loc!(l,
        fr: format!("!ROM lue, mais le checksum de l'en-tête diffère (fréquent sur les ROM amateur) : {file}, CRC32 {crc:08x}"),
        en: format!("!ROM read, but the header checksum differs (common on homebrew ROMs): {file}, CRC32 {crc:08x}"),
        ja: format!("!ROMを読み出しましたが、ヘッダーのチェックサムが異なります（自作ROMでは一般的）：{file}、CRC32 {crc:08x}"),
        zh: format!("!ROM已读取，但文件头校验和不一致（自制ROM常见）：{file}，CRC32 {crc:08x}"))
}
pub fn md_no_save_declared(l: Lang) -> String {
    loc!(l,
        fr: "l'en-tête ne déclare aucune sauvegarde",
        en: "the header declares no save",
        ja: "ヘッダーにセーブの記載がありません",
        zh: "文件头未声明存档")
    .into()
}
pub fn md_board(l: Lang) -> String {
    loc!(l, fr: "Carte : MD Flasher", en: "Board: MD Flasher", ja: "ボード：MD Flasher", zh: "开发板：MD Flasher").into()
}
pub fn no_flash_answers(l: Lang) -> [String; 2] {
    [
        loc!(l, fr: "Aucune puce flash ne répond :", en: "No flash chip answers:", ja: "フラッシュチップが応答しません：", zh: "没有闪存芯片响应：").into(),
        loc!(l, fr: "ROM masquée, non réinscriptible.", en: "mask ROM, cannot be rewritten.", ja: "マスクROMのため書き換え不可。", zh: "掩膜ROM，不可改写。").into(),
    ]
}
pub fn cfi_answers(l: Lang, ok: bool) -> String {
    let state = if ok {
        loc!(l, fr: "répond", en: "answers", ja: "応答あり", zh: "有响应")
    } else {
        loc!(l, fr: "ne répond pas", en: "does not answer", ja: "応答なし", zh: "无响应")
    };
    format!("CFI : {state}")
}
pub fn chip_size(l: Lang, mib: usize) -> String {
    loc!(l, fr: format!("Puce : {mib} Mo"), en: format!("Chip: {mib} MiB"), ja: format!("チップ：{mib} MiB"), zh: format!("芯片：{mib} MiB"))
}
pub fn maker_and_chip(l: Lang, maker: u16, ids: &str) -> String {
    loc!(l,
        fr: format!("Fabricant 0x{maker:04X}, puce {ids}"),
        en: format!("Maker 0x{maker:04X}, chip {ids}"),
        ja: format!("メーカー 0x{maker:04X}、チップ {ids}"),
        zh: format!("厂商 0x{maker:04X}，芯片 {ids}"))
}
pub fn byte_wide_pair(l: Lang) -> String {
    loc!(l,
        fr: "Deux puces 8 bits (cartouche de copie)",
        en: "Two 8-bit chips (repro cartridge)",
        ja: "8ビットチップ2個（コピーカート）",
        zh: "两个8位芯片（复刻卡带）")
    .into()
}
pub fn upper_writable(l: Lang) -> String {
    loc!(l,
        fr: "Au-delà de 4 Mo : accessible en écriture",
        en: "Beyond 4 MiB: writable",
        ja: "4 MiB超：書き込み可能",
        zh: "超过4 MiB的部分：可写")
    .into()
}
pub fn upper_dead(l: Lang) -> String {
    loc!(l,
        fr: "!Au-delà de 4 Mo : inaccessible (cartouche câblée en 4 Mo)",
        en: "!Beyond 4 MiB: unreachable (cartridge wired for 4 MiB)",
        ja: "!4 MiB超：アクセス不可（4 MiB配線のカート）",
        zh: "!超过4 MiB的部分：无法访问（卡带仅接线4 MiB）")
    .into()
}
pub fn upper_untested(l: Lang) -> String {
    loc!(l,
        fr: "Au-delà de 4 Mo : testé quand on écrit une ROM plus grande",
        en: "Beyond 4 MiB: tested when a larger ROM is written",
        ja: "4 MiB超：より大きなROMを書き込む際にテスト",
        zh: "超过4 MiB的部分：写入更大的ROM时测试")
    .into()
}
pub fn sectors_line(l: Lang, desc: &str) -> String {
    loc!(l, fr: format!("Secteurs : {desc}"), en: format!("Sectors: {desc}"), ja: format!("セクター：{desc}"), zh: format!("扇区：{desc}"))
}

// ---- Game Boy / GBA analysis ----
pub fn profiles_missing(l: Lang, dir: &str) -> String {
    loc!(l,
        fr: format!("profils de puces introuvables dans {dir}"),
        en: format!("chip profiles not found in {dir}"),
        ja: format!("{dir} にチッププロファイルが見つかりません"),
        zh: format!("在 {dir} 中找不到芯片配置"))
}
pub fn mode_line(l: Lang, agb: bool, volts: bool) -> String {
    let text = match (agb, volts) {
        (true, true) => loc!(l, fr: "GBA, 3,3 V", en: "GBA, 3.3 V", ja: "GBA、3.3 V", zh: "GBA，3.3 V"),
        (false, true) => loc!(l, fr: "Game Boy, 5 V", en: "Game Boy, 5 V", ja: "Game Boy、5 V", zh: "Game Boy，5 V"),
        (true, false) => "GBA (Chisflash)",
        (false, false) => "Game Boy (Chisflash)",
    };
    loc!(l, fr: format!("Mode : {text}"), en: format!("Mode: {text}"), ja: format!("モード：{text}"), zh: format!("模式：{text}"))
}
pub fn chip_answer(l: Lang, line: &str) -> String {
    loc!(l, fr: format!("Réponse {line}"), en: format!("Answer {line}"), ja: format!("応答 {line}"), zh: format!("响应 {line}"))
}
pub fn no_exact_profile(l: Lang) -> String {
    loc!(l,
        fr: "Aucun profil exact. Profil probable :",
        en: "No exact profile. Likely profile:",
        ja: "完全一致するプロファイルなし。有力な候補：",
        zh: "没有完全匹配的配置。可能的配置：")
    .into()
}
pub fn no_known_profile(l: Lang) -> String {
    loc!(l,
        fr: "!Aucun profil connu pour cette puce.",
        en: "!No known profile for this chip.",
        ja: "!このチップに該当するプロファイルがありません。",
        zh: "!没有适用于该芯片的已知配置。")
    .into()
}
pub fn profile_recognised(l: Lang, one: bool) -> String {
    if one {
        loc!(l, fr: "Profil reconnu :", en: "Profile recognised:", ja: "プロファイル認識：", zh: "已识别配置：").into()
    } else {
        loc!(l, fr: "Profils compatibles :", en: "Compatible profiles:", ja: "互換プロファイル：", zh: "兼容的配置：").into()
    }
}
pub fn more_profiles(l: Lang, n: usize) -> String {
    loc!(l, fr: format!("  (+ {n} autres)"), en: format!("  (+ {n} more)"), ja: format!("  （ほか{n}件）"), zh: format!("  （另有{n}个）"))
}
pub fn write_pin(l: Lang, pin: &str) -> String {
    loc!(l, fr: format!("Broche d'écriture : {pin}"), en: format!("Write pin: {pin}"), ja: format!("書き込みピン：{pin}"), zh: format!("写入引脚：{pin}"))
}
pub fn cfi_unreadable(l: Lang) -> String {
    loc!(l,
        fr: "!CFI : non lue (puce muette ou non standard).",
        en: "!CFI: not read (silent or non-standard chip).",
        ja: "!CFI：読めません（応答なし、または非標準のチップ）。",
        zh: "!CFI：无法读取（芯片无响应或不标准）。")
    .into()
}
pub fn cfi_summary(l: Lang, mib: u64, vmin: f64, vmax: f64) -> String {
    loc!(l,
        fr: format!("CFI : {mib} Mo, {vmin:.1} à {vmax:.1} V"),
        en: format!("CFI: {mib} MiB, {vmin:.1} to {vmax:.1} V"),
        ja: format!("CFI：{mib} MiB、{vmin:.1}〜{vmax:.1} V"),
        zh: format!("CFI：{mib} MiB，{vmin:.1}至{vmax:.1} V"))
}
pub fn buffer_write(l: Lang, bytes: Option<usize>) -> String {
    match bytes {
        Some(b) => loc!(l,
            fr: format!("Écriture par tampon : {b} octets"),
            en: format!("Buffered write: {b} bytes"),
            ja: format!("バッファ書き込み：{b}バイト"),
            zh: format!("缓冲写入：{b}字节")),
        None => loc!(l, fr: "Écriture par tampon : non".to_string(), en: "Buffered write: no".to_string(), ja: "バッファ書き込み：なし".to_string(), zh: "缓冲写入：无".to_string()),
    }
}
pub fn crossed_data_lines(l: Lang, pins: &str) -> String {
    loc!(l,
        fr: format!("!Lignes de données croisées : {pins}"),
        en: format!("!Crossed data lines: {pins}"),
        ja: format!("!データ線が入れ替わっています：{pins}"),
        zh: format!("!数据线交叉：{pins}"))
}

// ---- GBA flash ----
pub fn gba_bad_file(l: Lang) -> String {
    loc!(l,
        fr: "ce fichier n'est pas une ROM GBA valide (en-tête incorrect)",
        en: "this file is not a valid GBA ROM (bad header)",
        ja: "有効なGBA ROMではありません（ヘッダー不正）",
        zh: "该文件不是有效的GBA ROM（文件头错误）")
    .into()
}
pub fn chip_not_identified(l: Lang) -> String {
    loc!(l,
        fr: "puce non identifiée (pas de CFI) : rien n'est écrit",
        en: "chip not identified (no CFI): nothing written",
        ja: "チップを識別できません（CFIなし）：何も書き込みません",
        zh: "无法识别芯片（没有CFI）：未写入任何内容")
    .into()
}
pub fn backup_chip_progress(l: Lang) -> String {
    loc!(l,
        fr: "Sauvegarde de la puce avant écriture",
        en: "Backing up the chip before writing",
        ja: "書き込み前にチップをバックアップ中",
        zh: "写入前正在备份芯片")
    .into()
}
pub fn writing_gba(l: Lang, m: &str) -> String {
    loc!(l,
        fr: format!("Écriture GBA, ne pas débrancher : {m}"),
        en: format!("Writing GBA, do not unplug: {m}"),
        ja: format!("GBA書き込み中、抜かないでください：{m}"),
        zh: format!("正在写入GBA，请勿拔出：{m}"))
}
pub fn old_chip_in(l: Lang, e: &str, name: Option<&str>) -> String {
    format!("{e} ; {}", old_chip_tail(l, name))
}
pub fn gba_written_ok(l: Lang, attempts: usize, repaired: usize, name: Option<&str>) -> String {
    let head = loc!(l,
        fr: format!("ROM GBA écrite et vérifiée ({attempts} essai, {repaired} octets réparés)"),
        en: format!("GBA ROM written and verified ({attempts} attempt, {repaired} bytes repaired)"),
        ja: format!("GBA ROMを書き込み、検証しました（{attempts}回、{repaired}バイト修復）"),
        zh: format!("GBA ROM已写入并验证（{attempts}次尝试，修复{repaired}字节）"));
    format!("{head} ; {}", old_chip_tail(l, name))
}

// ---- Game Boy flash ----
pub fn only_mbc5(l: Lang) -> String {
    loc!(l,
        fr: "seul le flashage de cartouches MBC5 est géré",
        en: "only MBC5 cartridges can be flashed",
        ja: "書き込めるのはMBC5カートのみです",
        zh: "仅支持烧录MBC5卡带")
    .into()
}
pub fn gb_bad_size(l: Lang) -> String {
    loc!(l,
        fr: "taille de fichier invalide pour une ROM Game Boy",
        en: "invalid file size for a Game Boy ROM",
        ja: "Game Boy ROMとして不正なファイルサイズです",
        zh: "文件大小不符合Game Boy ROM")
    .into()
}
pub fn gb_bad_file(l: Lang) -> String {
    loc!(l,
        fr: "ce fichier n'est pas une ROM Game Boy valide (logo absent)",
        en: "this file is not a valid Game Boy ROM (no logo)",
        ja: "有効なGame Boy ROMではありません（ロゴなし）",
        zh: "该文件不是有效的Game Boy ROM（没有标志）")
    .into()
}
pub fn chip_not_recognised(l: Lang) -> String {
    loc!(l,
        fr: "puce flash non reconnue (ou ambiguë) : rien n'est écrit",
        en: "flash chip not recognised (or ambiguous): nothing written",
        ja: "フラッシュチップを認識できません（または曖昧）：何も書き込みません",
        zh: "无法识别闪存芯片（或结果不唯一）：未写入任何内容")
    .into()
}
pub fn failed_backup_in(l: Lang, e: &str, path: Option<&str>) -> String {
    match path {
        Some(path) => loc!(l,
            fr: format!("{e} : sauvegarde dans {path}"),
            en: format!("{e}: backup in {path}"),
            ja: format!("{e}：バックアップは {path}"),
            zh: format!("{e}：备份位于 {path}")),
        None => format!("{e} ; {}", old_rom_tail(l, None)),
    }
}
pub fn gb_written_ok(l: Lang, rewritten: usize, name: Option<&str>) -> String {
    let head = loc!(l,
        fr: format!("ROM écrite et vérifiée ({rewritten} secteurs)"),
        en: format!("ROM written and verified ({rewritten} sectors)"),
        ja: format!("ROMを書き込み、検証しました（{rewritten}セクター）"),
        zh: format!("ROM已写入并验证（{rewritten}个扇区）"));
    format!("{head} ; {}", old_rom_tail(l, name))
}

// ---- save writing ----
pub fn save_size_mismatch(l: Lang, file: usize, save: usize) -> String {
    loc!(l,
        fr: format!("le fichier fait {file} octets, la sauvegarde {save}"),
        en: format!("the file is {file} bytes, the save is {save}"),
        ja: format!("ファイルは{file}バイト、セーブは{save}バイトです"),
        zh: format!("文件为{file}字节，存档为{save}字节"))
}
pub fn backing_up_old_save(l: Lang) -> &'static str {
    loc!(l,
        fr: "Sauvegarde de l'ancienne sauvegarde",
        en: "Backing up the old save",
        ja: "旧セーブをバックアップ中",
        zh: "正在备份旧存档")
}
pub fn writing_save(l: Lang) -> &'static str {
    loc!(l, fr: "Écriture de la sauvegarde", en: "Writing the save", ja: "セーブを書き込み中", zh: "正在写入存档")
}
pub fn note_old_save(l: Lang, file: &str) -> String {
    loc!(l,
        fr: format!(" ; ancienne : backups/{file}"),
        en: format!("; old: backups/{file}"),
        ja: format!("；旧セーブ：backups/{file}"),
        zh: format!("；旧存档：backups/{file}"))
}
pub fn note_old_save_unstable(l: Lang) -> String {
    loc!(l,
        fr: " ; ancienne sauvegarde instable, non gardée".to_string(),
        en: "; the old save reads unstably, not kept".to_string(),
        ja: "；旧セーブは読み出しが不安定なため保存しません".to_string(),
        zh: "；旧存档读取不稳定，未保留".to_string())
}
pub fn save_readback_differs(l: Lang, note: &str) -> String {
    loc!(l,
        fr: format!("la relecture diffère de ce qui a été écrit{note}"),
        en: format!("the read-back differs from what was written{note}"),
        ja: format!("読み戻しが書き込んだ内容と一致しません{note}"),
        zh: format!("回读结果与写入内容不一致{note}"))
}
pub fn save_written_unverified(l: Lang, note: &str) -> String {
    loc!(l,
        fr: format!("!Attention : sauvegarde écrite et relue dans la même session, conservation NON vérifiée (pas de coupure possible){note}"),
        en: format!("!Warning: save written and read back in the same session, retention NOT verified (no power cut possible){note}"),
        ja: format!("!注意：セーブを書き込み、同一セッションで読み戻しました。保持は未検証です（電源を切れません）{note}"),
        zh: format!("!注意：存档已写入并在同一次会话中回读，保存情况未验证（无法断电）{note}"))
}
pub fn save_written_ok(l: Lang, note: &str) -> String {
    loc!(l,
        fr: format!("Sauvegarde écrite, conservée après coupure{note}"),
        en: format!("Save written, kept after a power cut{note}"),
        ja: format!("セーブを書き込みました。電源断後も保持されています{note}"),
        zh: format!("存档已写入，断电后仍然保留{note}"))
}
pub fn save_lost_at_cut(l: Lang, note: &str) -> String {
    loc!(l,
        fr: format!("écrite mais perdue à la coupure : pile morte ?{note}"),
        en: format!("written but lost at the power cut: dead battery?{note}"),
        ja: format!("書き込みましたが、電源断で失われました：電池切れ？{note}"),
        zh: format!("已写入，但断电后丢失：电池耗尽？{note}"))
}

// ---- reading ----
pub fn rom_size_unknown_db(l: Lang) -> String {
    loc!(l,
        fr: "taille de ROM inconnue (jeu absent de la base)",
        en: "unknown ROM size (game not in the database)",
        ja: "ROMサイズが不明です（ゲームがデータベースにありません）",
        zh: "ROM大小未知（游戏不在数据库中）")
    .into()
}
pub fn rom_verified_crc(l: Lang, file: &str) -> String {
    loc!(l, fr: format!("ROM vérifiée (CRC base OK) : {file}"), en: format!("ROM verified (database CRC OK): {file}"), ja: format!("ROMを検証しました（DBのCRC一致）：{file}"), zh: format!("ROM已验证（数据库CRC一致）：{file}"))
}
pub fn rom_crc_differs(l: Lang, file: &str) -> String {
    loc!(l,
        fr: format!("CRC différent de la base : {file} peut être corrompu"),
        en: format!("CRC differs from the database: {file} may be corrupt"),
        ja: format!("CRCがデータベースと異なります：{file}は破損している可能性があります"),
        zh: format!("CRC与数据库不一致：{file}可能已损坏"))
}
pub fn rom_verified_header(l: Lang, file: &str) -> String {
    loc!(l,
        fr: format!("ROM vérifiée (checksum de l'en-tête OK) : {file}"),
        en: format!("ROM verified (header checksum OK): {file}"),
        ja: format!("ROMを検証しました（ヘッダーのチェックサム一致）：{file}"),
        zh: format!("ROM已验证（文件头校验和一致）：{file}"))
}
pub fn rom_corrected(l: Lang, file: &str, banks: usize) -> String {
    loc!(l,
        fr: format!("ROM vérifiée (checksum de l'en-tête OK) après relecture de {banks} banque(s) mal lue(s) : {file}"),
        en: format!("ROM verified (header checksum OK) after re-reading {banks} badly read bank(s): {file}"),
        ja: format!("ROMを検証しました（ヘッダーのチェックサム一致）。誤読した{banks}バンクを再読込：{file}"),
        zh: format!("ROM已验证（文件头校验和一致），已重读{banks}个读错的存储块：{file}"))
}
pub fn rom_stable_checksum_differs(l: Lang, file: &str) -> String {
    loc!(l,
        fr: format!("!ROM lue deux fois à l'identique, mais le checksum de l'en-tête diffère (fréquent sur les ROM amateur) : {file}"),
        en: format!("!ROM read twice with the same result, but the header checksum differs (common on homebrew ROMs): {file}"),
        ja: format!("!ROMを2回読み出して同じ結果でしたが、ヘッダーのチェックサムが異なります（自作ROMではよくあります）：{file}"),
        zh: format!("!ROM两次读取结果一致，但文件头校验和不同（自制ROM常见）：{file}"))
}
pub fn rom_header_checksum_wrong(l: Lang, file: &str) -> String {
    loc!(l,
        fr: format!("checksum de l'en-tête faux : {file} peut être corrompu"),
        en: format!("header checksum wrong: {file} may be corrupt"),
        ja: format!("ヘッダーのチェックサムが不一致：{file}は破損している可能性があります"),
        zh: format!("文件头校验和错误：{file}可能已损坏"))
}
pub fn rom_unverifiable(l: Lang, file: &str) -> String {
    loc!(l, fr: format!("ROM lue, non vérifiable : {file}"), en: format!("ROM read, cannot be verified: {file}"), ja: format!("ROMを読み出しました（検証不可）：{file}"), zh: format!("ROM已读取（无法验证）：{file}"))
}
pub fn reading_save_two_passes(l: Lang, title: &str) -> String {
    loc!(l,
        fr: format!("Lecture sauvegarde (2 passes) : {title}"),
        en: format!("Reading save (2 passes): {title}"),
        ja: format!("セーブ読み出し（2回）：{title}"),
        zh: format!("读取存档（2遍）：{title}"))
}
pub fn save_lost_after_cut(l: Lang, diff: usize) -> String {
    loc!(l,
        fr: format!("données perdues à la coupure ({diff} octets) : pile morte ? rien enregistré"),
        en: format!("data lost at the power cut ({diff} bytes): dead battery? nothing saved"),
        ja: format!("電源断でデータが失われました（{diff}バイト）：電池切れ？何も保存しません"),
        zh: format!("断电后数据丢失（{diff}字节）：电池耗尽？未保存任何内容"))
}
pub fn save_unstable(l: Lang, diff: usize) -> String {
    loc!(l,
        fr: format!("lecture instable ({diff} octets diffèrent) : rien enregistré"),
        en: format!("unstable read ({diff} bytes differ): nothing saved"),
        ja: format!("読み出しが不安定です（{diff}バイトが異なります）：何も保存しません"),
        zh: format!("读取不稳定（{diff}字节不同）：未保存任何内容"))
}
pub fn save_read_ok(l: Lang, file: &str) -> String {
    loc!(l,
        fr: format!("Sauvegarde lue (identique après coupure d'alimentation) : {file}"),
        en: format!("Save read (identical after a power cut): {file}"),
        ja: format!("セーブを読み出しました（電源断後も同一）：{file}"),
        zh: format!("存档已读取（断电后内容一致）：{file}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cancelled_message_exists_in_every_language_and_is_distinct() {
        let all: Vec<String> = [Lang::Fr, Lang::En, Lang::Ja, Lang::Zh].iter().map(|&l| cancelled(l)).collect();
        for (i, a) in all.iter().enumerate() {
            assert!(!a.is_empty());
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_warning_stays_a_warning_in_every_language() {
        for l in [Lang::Fr, Lang::En, Lang::Ja, Lang::Zh] {
            assert!(unverified_save(l, "f").starts_with('!'));
            assert!(md_rom_read_checksum_differs(l, "f", 1).starts_with('!'));
            assert!(save_written_unverified(l, "").starts_with('!'));
            assert!(upper_dead(l).starts_with('!'));
            assert!(!md_rom_read_ok(l, "f", 1).starts_with('!'));
        }
    }
}

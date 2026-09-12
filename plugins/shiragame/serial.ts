/** The producer's platform-specific rules, preserving exact serial fallback.
 * https://github.com/SnowflakePowered/shiratsu/blob/master/shiratsu-dat/src/common/serial.rs
 */
export class Serial {
  static normalize(platform: string, serial: string): string {
    if (/^SONY_(PSX|PS2|PS3|PS4|PSP|PSV)$/.test(platform)) {
      // A flat suffix character class avoids the upstream regex's nested repetition.
      return serial.replace(/^([a-zA-Z]+)[-_ ](\d+)[\w# /-]*$/, "$1-$2");
    }
    if (/^SEGA_(GEN|CD|DC|GG|SAT|32X|32X_CD)$/.test(platform)) {
      const rules: readonly [RegExp, string][] = [
        [/^([\w]+)-([\w]+)(-[\w.]+)$/, "$1-$2"],
        [/^(MK|T|GS)([\w]+)(-[\w.]+)?$/, "$1-$2"],
        [/^(0{2,3})(\d+)(-\d{2}\w?)?$/, "$1$2"],
      ];
      for (const [pattern, replacement] of rules) {
        if (pattern.test(serial)) return serial.replace(pattern, replacement);
      }
      return serial;
    }
    if (platform === "NEC_TGCD") return serial.replace(/^(\w{4,5})[ -](\w+)$/, "$1$2");
    const nintendo: Readonly<Record<string, RegExp>> = {
      NINTENDO_GCN: /^DL-DOL-(\w{4})-[-\w()]+$/,
      NINTENDO_WII: /^RVL-(\w{4})-[-\w()]+$/,
      NINTENDO_WIIU: /^WUP-(?:P|M|N|T|U|B)-(\w{4})-[-\w()]+$/,
      NINTENDO_3DS: /^CTR-(?:P|M|N|T|U|B)-(\w{4})(?:-[-\w()]+)*$/,
      NINTENDO_NSW: /^LA-H-(\w{5})(?:-[-\w()]+)*$/,
    };
    return nintendo[platform]?.exec(serial)?.[1] ?? serial;
  }
}

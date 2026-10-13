export type DigestAlgorithm = "sha2" | "sha1" | "md5" | "crc32" | "blake3";
/** On-demand lowercase hexadecimal digest. sha2 means SHA-256. */
export interface Digestible {
  digest(algorithm: DigestAlgorithm): Promise<string>;
}

/** Serializable game metadata shared by frontend and plugin APIs. */
export type Package = Readonly<{
  uri: string;
  filename: string;
  platform: string;
  serial: string | null;
  titleId: string | null;
  /**
   * Set at launch for a title that lives inside console storage, installed or mounted in place:
   * the storage device, the title's path inside it, and the file to boot relative to that path.
   */
  installed?: Readonly<{ device: string; path: string; entrypoint: string }> | null;
  bundle?: Readonly<{
    revision: string;
    entrypoint: string;
    files: readonly Readonly<{ name: string; uri: string | null; text: string | null }>[];
  }> | null;
}>;

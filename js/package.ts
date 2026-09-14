/** Serializable game metadata shared by frontend and plugin APIs. */
export type Package = Readonly<{
  filename: string;
  platform: string;
  serial: string | null;
  titleId: string | null;
  binaryDigest: string;
  bundle?: Readonly<{
    revision: string;
    entrypoint: string;
    files: readonly Readonly<{ name: string; path: string | null; text: string | null }>[];
  }> | null;
}>;

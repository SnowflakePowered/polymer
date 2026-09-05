/** Serializable game metadata shared by frontend and plugin APIs. */
export type Package = Readonly<{
  filename: string;
  platform: string;
  serial: string | null;
  titleId: string | null;
  binaryDigest: string;
}>;

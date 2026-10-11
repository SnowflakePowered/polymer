export type ConfigurationChoice = Readonly<{
  key: string;
  label: string;
}>;

export type ConfigurationLayoutOption = Readonly<{
  key: string;
  title: string;
  description?: string;
  type: "bool" | "int" | "float" | "string" | "path" | "select";
  default: boolean | number | string | null;
  min?: number;
  max?: number;
  choices?: readonly ConfigurationChoice[];
}>;

export type ConfigurationGroup<Option = ConfigurationLayoutOption> = Readonly<{
  key: string;
  title: string;
  description?: string;
  options: readonly Option[];
}>;

export type ConfigurationLayout<Option = ConfigurationLayoutOption> = Readonly<{
  schema: string;
  groups: readonly ConfigurationGroup<Option>[];
}>;

export type UserConfiguration = Readonly<
  Record<string, Readonly<Record<string, boolean | number | string | null>>>
>;

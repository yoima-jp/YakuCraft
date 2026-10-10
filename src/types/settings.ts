export interface AppSettingsDto extends Record<string, unknown> {
  schemaVersion: number;
}

export interface AppInfoDto {
  appName: string;
  version: string;
  dataDirectory: string;
}

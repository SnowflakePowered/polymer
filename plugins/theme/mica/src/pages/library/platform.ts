export const platformName = (platform: string) => window.Polymer.machine(platform)?.name ?? platform;

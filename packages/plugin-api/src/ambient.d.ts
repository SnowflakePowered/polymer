declare module "pkg:*.toml" {
    const configuration: Readonly<Record<string, any>>;
    export default configuration;
}

declare module "pkg:*" {
    const resource: import("polymer:api").DirEntry;
    export default resource;
}

declare module "jsr:*";

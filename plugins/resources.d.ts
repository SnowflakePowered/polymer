declare module "resources:*" {
    const resource: import("./plugin-api.ts").DirectoryResource;
    export default resource;
}

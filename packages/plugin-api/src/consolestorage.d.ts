/** Read-only inspection of configured disk paths or pkg: resources. */
export const resources: { inspect(path: string): { exists: boolean; size?: number; directory?: boolean } };

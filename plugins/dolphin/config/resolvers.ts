const resolveGraphicsBackend = {
    depends: [],
    resolve: (ctx: any) => {
        return [ 
            {
                value: "D3D",
                label: "Direct3D 11"
            },
            {
                value: "OGL",
                label: "OpenGL"
            }
        ]
    }
}

export { resolveGraphicsBackend }
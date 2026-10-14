# Polymer TypeScript packages

| Package | Contents | Used by |
|---|---|---|
| `@polymer-fe/types` | Data types shared by plugins and the frontend (`Package`, `Digestible`, configuration layouts). | both |
| `@polymer-fe/plugin-api` | Declarations for the host modules plugins import (`polymer:api`, `polymer:plugin`, `polymer:jobs`, ...) and for `pkg:` and `jsr:` imports. | plugins |
| `@polymer-fe/emulator-updates` | Emulator binary updates from the emulator index: download, extract, stage, and promote at launch. Published to JSR; plugins import `jsr:@polymer-fe/emulator-updates`, which development builds resolve to this directory. | plugins |
| `@polymer-fe/frontend` | The `window.Polymer` API and theme-independent shell helpers (`LibraryState`, `NavigationRouter`, `GameLauncher`, ...). | themes |
| `@polymer-fe/solid`, `@polymer-fe/react`, `@polymer-fe/vue` | JSX declarations for Polymer's custom elements (`<tracked-surface>`) in each framework. Type-only, with the framework as a peer dependency. | themes |

The packages are TypeScript source; there is no build step.

## Plugins

Each plugin directory has a `tsconfig.json` that extends `packages/plugin-api/tsconfig.plugin.json` and maps `polymer:*`, `@polymer-fe/types` and the plugin's own `pkg:*.ts` modules:

```json
{
  "extends": "../../packages/plugin-api/tsconfig.plugin.json",
  "compilerOptions": {
    "paths": {
      "polymer:*": ["../../packages/plugin-api/src/*"],
      "@polymer-fe/types": ["../../packages/types/src/index.ts"],
      "pkg:*.ts": ["./*.ts"]
    }
  },
  "include": ["**/*.ts", "../../packages/plugin-api/src/ambient.d.ts"],
  "exclude": ["bin"]
}
```

Type-check every plugin with `npm install` and `npm run check` in `packages/plugin-api`.

## Themes

Themes depend on both frontend packages and set `"preserveSymlinks": true` in their tsconfig, because npm does not install the dependencies of linked packages:

```json
"@polymer-fe/frontend": "file:../../../packages/frontend",
"@polymer-fe/types": "file:../../../packages/types"
```

Import the API from `@polymer-fe/frontend` and helpers from `@polymer-fe/frontend/<module>`, for example `@polymer-fe/frontend/library-state`.

`HTMLElementTagNameMap` already types `tracked-surface` for DOM APIs. To use it as a JSX tag, add the package for your framework and list it in the tsconfig `types`, for example `"@polymer-fe/solid": "file:../../../packages/solid"` and `"types": ["vite/client", "@polymer-fe/solid"]`.

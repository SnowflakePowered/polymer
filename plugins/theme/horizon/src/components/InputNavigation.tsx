import { useShell } from "../app/shell-context";
import { useInputScope } from "../ui/input";
/** Screen fallback; overlays and controls register more specific scopes. */
export function InputNavigation() {
  const shell = useShell();
  useInputScope(() => ({ root: document.body, enabled: !shell.model.launch, navigation: true }));
  return null;
}

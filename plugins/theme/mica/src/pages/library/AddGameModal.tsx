import { createEffect, on } from "solid-js";
import { useShell } from "../../app/shell";
import { Button, Icon, Modal } from "../../ui";

export function AddGameModal(props: { open: boolean; onClose(): void }) {
  const shell = useShell();
  createEffect(on(() => shell.state.activities.length, (count, previous) => { if (props.open && previous !== undefined && count > previous) props.onClose(); }));
  const pick = (picker: () => Promise<readonly FileSystemHandle[]>) => void picker()
    .then(handles => { for (const handle of handles) void shell.library.import(handle); if (handles.length) props.onClose(); })
    .catch(error => { if (!(error instanceof DOMException && error.name === "AbortError")) shell.fail(error); });
  return <Modal open={props.open} onClose={props.onClose} class="mc-add-game" title="Add a game to your library"
    buttons={<Button onClick={props.onClose}>Cancel</Button>}>
    <div class="mc-drop-area">
      <Icon name="upload" size={40} color="#c5c7cb" />
      <strong>Drop games here</strong>
      <span>Drop a game file, folder or archive anywhere on this window.</span>
      <div class="mc-drop-actions">
        <Button primary data-autofocus onClick={() => pick(() => showOpenFilePicker({ multiple: true }))}><Icon name="document" size={18} />Browse files</Button>
        <Button onClick={() => pick(async () => [await showDirectoryPicker()])}><Icon name="folder" size={18} />Browse folder</Button>
      </div>
    </div>
  </Modal>;
}

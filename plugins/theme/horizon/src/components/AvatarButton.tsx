import { Icon } from "./Icon";
import "./AvatarButton.css";

export function AvatarButton() {
  return <button type="button" class="horizon-avatar" aria-label="Profile" title="Profile"><span><Icon name="user" /></span></button>;
}

import { Avatar, IconButton } from "../ui";
import "./AvatarButton.css";
export function AvatarButton() {
  return <IconButton class="horizon-avatar" label="Profile" disabled title="Profile"><Avatar label="Profile" tone="onBrand" /></IconButton>;
}

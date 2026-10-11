import { createEffect, onCleanup, onMount, untrack, type ParentProps } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { NotificationCenter, type Notification } from "../../../../../../js/notifications";
import { useShell } from "../../app/shell-context";
import { NotificationContext } from "./notification-context";

export function NotificationProvider(props: ParentProps) {
  const shell = useShell();
  const [items, setItems] = createStore<Record<string, Notification>>({});
  const notifications = new NotificationCenter(window.Polymer, next => setItems(reconcile(Object.fromEntries(next.map(item => [item.id, item])))));
  onCleanup(() => notifications.dispose());
  createEffect(() => {
    const activities = shell.model.libraryActivities.map(activity => ({ ...activity }));
    untrack(() => {
      for (const activity of activities) notifications.upsert({ ...activity, id: `library:${activity.id}` });
      if (!activities.some(item => item.id === "imports")) notifications.remove("library:imports");
    });
  });
  createEffect(() => {
    const message = shell.model.error;
    untrack(() => message ? notifications.upsert({ id: "error", title: "Something went wrong", message, state: "error" }) : notifications.remove("error"));
  });
  createEffect(() => {
    const toast = shell.model.controllerToast;
    const name = toast?.name, index = toast?.index;
    untrack(() => name !== undefined ? notifications.upsert({ id: "controller", title: name, message: `Connected as player ${index! + 1}`, state: "info" }) : notifications.remove("controller"));
  });
  onMount(() => notifications.start());
  return <NotificationContext.Provider value={{
    items: () => Object.values(items),
    dismiss: id => notifications.dismiss(id),
    reopen: id => notifications.reopen(id),
  }}>{props.children}</NotificationContext.Provider>;
}

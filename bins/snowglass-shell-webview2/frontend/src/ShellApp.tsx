import { useEffect, useState } from "react";
import "./gameview";
import "./shell.css";

export function ShellApp() {
  const [status, setStatus] = useState("Waiting for launch options");

  useEffect(() => {
    const listener = (event: Event) => {
      const message = (event as MessageEvent).data;
      if (message?.type === "gameViewStatus" && typeof message.status === "string") {
        setStatus(message.status);
      }
    };
    window.gameLayer.addEventListener("message", listener);
    return () => window.gameLayer.removeEventListener("message", listener);
  }, []);

  return (
    <main className="shell">
      <section className="surface-stage" />
      <output className="status">{status}</output>
    </main>
  );
}

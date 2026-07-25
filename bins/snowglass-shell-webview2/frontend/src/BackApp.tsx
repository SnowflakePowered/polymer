import { useEffect, useState } from "react";
import "./back.css";

export function BackApp() {
  const [hasMessage, setHasMessage] = useState(false);

  useEffect(() => {
    const receive = () => setHasMessage(true);
    addEventListener("message", receive);
    return () => removeEventListener("message", receive);
  }, []);

  return <main className="background" data-has-message={hasMessage || undefined} />;
}

import { Server } from "lucide-react";

/** Simple generic marks (not vendor logos): a penguin-free Linux terminal glyph and a four-pane window. */
export function OsIcon({ os, size = 20 }: { os?: string | null; size?: number }) {
  if (os === "windows")
    return (
      <svg width={size} height={size} viewBox="0 0 24 24" aria-label="Windows" role="img">
        <rect x="3" y="3" width="8" height="8" rx="1.5" fill="currentColor" opacity="0.9" />
        <rect x="13" y="3" width="8" height="8" rx="1.5" fill="currentColor" opacity="0.7" />
        <rect x="3" y="13" width="8" height="8" rx="1.5" fill="currentColor" opacity="0.7" />
        <rect x="13" y="13" width="8" height="8" rx="1.5" fill="currentColor" opacity="0.9" />
      </svg>
    );
  if (os === "linux")
    return (
      <svg width={size} height={size} viewBox="0 0 24 24" aria-label="Linux" role="img" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <rect x="3" y="4" width="18" height="16" rx="3" />
        <path d="M7 10l3 2-3 2M12 15h5" />
      </svg>
    );
  return <Server size={size} />;
}

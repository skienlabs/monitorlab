import { createContext, ReactNode, useContext } from "react";
import { api } from "./api";
import { usePoll } from "./components/ui";
import type { Server } from "./types";

interface Ctx {
  servers: Server[] | null;
  error: string | null;
  reload: () => Promise<void>;
}

const C = createContext<Ctx>({ servers: null, error: null, reload: async () => {} });
export const useServers = () => useContext(C);

/** Server list shared by the navigation rail and the overview, refreshed every 10s. */
export function ServersProvider({ children }: { children: ReactNode }) {
  const { data, error, reload } = usePoll(() => api<Server[]>("/servers"), [], 10_000);
  return <C.Provider value={{ servers: data, error, reload }}>{children}</C.Provider>;
}

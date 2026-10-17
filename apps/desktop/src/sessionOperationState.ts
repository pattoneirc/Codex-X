export type LoadingScope = "general" | "sessionSync";

/** Session maintenance still blocks other UI actions; only activation is exempt. */
export function providerActivationIsBusy(loadingScopes: Iterable<LoadingScope>, actions: Iterable<string>): boolean {
  for (const scope of loadingScopes) if (scope !== "sessionSync") return true;
  for (const action of actions) if (action !== "syncSessions") return true;
  return false;
}

export type SessionStatusContext = {
  configDir: string;
  targetProvider: string;
  providerRevision: number;
};

export function sessionStatusContextKey(context: SessionStatusContext): string {
  return JSON.stringify([context.configDir, context.targetProvider, context.providerRevision]);
}

export function createSessionStatusTracker() {
  let sequence = 0;
  return {
    invalidate() { sequence += 1; },
    begin(context: SessionStatusContext) {
      return Object.freeze({ sequence: ++sequence, context: Object.freeze({ ...context }) });
    },
    isCurrent(request: { sequence: number; context: SessionStatusContext }, current: SessionStatusContext | null) {
      return Boolean(current && request.sequence === sequence
        && request.context.configDir === current.configDir
        && request.context.targetProvider === current.targetProvider
        && request.context.providerRevision === current.providerRevision);
    },
  };
}

export async function readCurrentSessionStatus<T extends { targetProvider: string }>(
  tracker: ReturnType<typeof createSessionStatusTracker>,
  getContext: () => SessionStatusContext | null,
  read: (context: Readonly<SessionStatusContext>) => Promise<T>,
  accept: (status: T) => void,
): Promise<T | null> {
  const context = getContext();
  if (!context) return null;
  const request = tracker.begin(context);
  try {
    const status = await read(request.context);
    if (!tracker.isCurrent(request, getContext()) || status.targetProvider !== request.context.targetProvider) return null;
    accept(status);
    return status;
  } catch (error) {
    if (!tracker.isCurrent(request, getContext())) return null;
    throw error;
  }
}

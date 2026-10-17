// The single bridge to the host: every call is api(op, args).

export type Invoke = (command: string, payload: Record<string, unknown>) => Promise<unknown>;

/** An op failure as the host reports it: `{code, message}`. */
export class ApiError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
  }
}

export type Api = <T = unknown>(op: string, args?: unknown) => Promise<T>;

function toApiError(e: unknown): ApiError {
  if (e instanceof ApiError) return e;
  if (e && typeof e === 'object' && 'code' in e && 'message' in e) {
    const { code, message } = e as { code: unknown; message: unknown };
    return new ApiError(String(code), String(message));
  }
  return new ApiError('transport', e instanceof Error ? e.message : String(e));
}

/** Build the bridge over a Tauri-style `invoke`. Only the `api` command is ever called. */
export function createApi(invoke: Invoke): Api {
  return async <T,>(op: string, args: unknown = null): Promise<T> => {
    try {
      return (await invoke('api', { op, args })) as T;
    } catch (e) {
      throw toApiError(e);
    }
  };
}

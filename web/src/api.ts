// API client. Non-GET requests carry the X-RustSolar header (CSRF guard on the server).

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

let onUnauthorized: () => void = () => {};
export function setUnauthorizedHandler(f: () => void) {
  onUnauthorized = f;
}

async function handle<T>(r: Response): Promise<T> {
  if (r.status === 401) {
    onUnauthorized();
    throw new ApiError(401, 'login required');
  }
  const text = await r.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (!r.ok) {
    const msg = body && typeof body === 'object' && 'error' in body ? String((body as { error: unknown }).error) : `HTTP ${r.status}`;
    throw new ApiError(r.status, msg);
  }
  return body as T;
}

export async function get<T>(path: string): Promise<T> {
  return handle<T>(await fetch(`/api${path}`, { credentials: 'same-origin', cache: 'no-store' }));
}

export async function post<T>(path: string, body?: unknown): Promise<T> {
  return handle<T>(
    await fetch(`/api${path}`, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-RustSolar': '1' },
      body: JSON.stringify(body ?? {}),
    }),
  );
}

export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

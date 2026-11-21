// A thin typed fetch wrapper. Hand-written rather than generated from OpenAPI — the admin
// surface is a handful of endpoints, so a full spec + codegen would be more machinery than
// it saves.
//
// Any 401 (an expired or missing session) fires the registered handler, which bounces the
// SPA to the login page from anywhere.

let onUnauthorized: (() => void) | null = null

export function setUnauthorizedHandler(fn: () => void): void {
  onUnauthorized = fn
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: body !== undefined ? { 'Content-Type': 'application/json' } : {},
    body: body !== undefined ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  })

  if (res.status === 401) {
    onUnauthorized?.()
    throw new ApiError(401, 'not authenticated')
  }
  if (!res.ok) {
    let message = res.statusText
    try {
      const parsed = await res.json()
      if (parsed && typeof parsed.error === 'string') message = parsed.error
    } catch {
      // non-JSON error body; keep the status text
    }
    throw new ApiError(res.status, message)
  }

  if (res.status === 204) return undefined as T
  const contentType = res.headers.get('content-type') ?? ''
  return (contentType.includes('application/json') ? await res.json() : await res.text()) as T
}

export const api = {
  get: <T>(path: string) => request<T>('GET', path),
  post: <T>(path: string, body?: unknown) => request<T>('POST', path, body),
  put: <T>(path: string, body?: unknown) => request<T>('PUT', path, body),
}

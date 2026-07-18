import type { ApiErrorBody } from '@/types/jellyfin';

const TOKEN_STORAGE_KEY = 'jellyfin_rs_admin_token';
const DEVICE_ID_STORAGE_KEY = 'jellyfin_rs_admin_device_id';

type RequestOptions = Omit<RequestInit, 'body'> & {
  body?: BodyInit | Record<string, unknown> | unknown[] | null;
  token?: string | null;
};

export class HttpError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = 'HttpError';
    this.status = status;
  }
}

function configuredBaseUrl() {
  return import.meta.env.VITE_JELLYFIN_API_BASE.trim().replace(/\/$/, '');
}

function apiBaseUrl() {
  const configured = configuredBaseUrl();
  if (import.meta.env.DEV && configured) {
    return '/api';
  }
  return configured;
}

function requestUrl(path: string) {
  const normalizedPath = path.startsWith('/') ? path : `/${path}`;
  return `${apiBaseUrl()}${normalizedPath}`;
}

async function responseError(response: Response) {
  const contentType = response.headers.get('content-type') ?? '';
  if (contentType.includes('application/json')) {
    const body = (await response.json().catch(() => null)) as ApiErrorBody | null;
    const message = body?.Error || body?.Message;
    if (message) {
      return message;
    }
  }

  const text = await response.text().catch(() => '');
  return text || `HTTP ${response.status}`;
}

export async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { body, headers, token, ...init } = options;
  const requestHeaders = new Headers(headers);
  requestHeaders.set('Accept', 'application/json');

  if (token) {
    requestHeaders.set('X-Emby-Token', token);
  }

  let requestBody = body as BodyInit | null | undefined;
  if (body && typeof body === 'object' && !(body instanceof FormData) && !(body instanceof Blob)) {
    requestHeaders.set('Content-Type', 'application/json');
    requestBody = JSON.stringify(body);
  }

  const response = await fetch(requestUrl(path), {
    ...init,
    body: requestBody,
    headers: requestHeaders
  });

  if (!response.ok) {
    throw new HttpError(await responseError(response), response.status);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  const text = await response.text();
  if (!text) {
    return undefined as T;
  }

  return JSON.parse(text) as T;
}

export function tokenStorage() {
  return {
    get: () => localStorage.getItem(TOKEN_STORAGE_KEY),
    set: (token: string) => localStorage.setItem(TOKEN_STORAGE_KEY, token),
    remove: () => localStorage.removeItem(TOKEN_STORAGE_KEY)
  };
}

export function deviceId() {
  const existing = localStorage.getItem(DEVICE_ID_STORAGE_KEY);
  if (existing) {
    return existing;
  }

  const value = crypto.randomUUID();
  localStorage.setItem(DEVICE_ID_STORAGE_KEY, value);
  return value;
}

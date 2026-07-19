import { request } from '@/services/http';
import type { ApiKeyQueryResult, DoubanClientConfiguration, TmdbClientConfiguration } from '@/types/settings';

export function tmdbClientConfiguration(token: string) {
  return request<TmdbClientConfiguration>('/Tmdb/ClientConfiguration', { token });
}

export function updateTmdbApiKey(token: string, apiKey: string) {
  return request<void>('/System/Configuration/TmdbApiKey', {
    method: 'POST',
    token,
    body: {
      TmdbApiKey: apiKey
    }
  });
}

export function doubanClientConfiguration(token: string) {
  return request<DoubanClientConfiguration>('/Douban/ClientConfiguration', { token });
}

export function updateDoubanCookie(token: string, cookie: string) {
  return request<void>('/System/Configuration/DoubanCookie', {
    method: 'POST',
    token,
    body: {
      DoubanCookie: cookie
    }
  });
}

export function apiKeys(token: string) {
  return request<ApiKeyQueryResult>('/Auth/Keys', { token });
}

export function createApiKey(token: string, appName: string) {
  return request<void>(`/Auth/Keys?app=${encodeURIComponent(appName)}`, {
    method: 'POST',
    token
  });
}

export function deleteApiKey(token: string, accessToken: string) {
  return request<void>(`/Auth/Keys/${encodeURIComponent(accessToken)}`, {
    method: 'DELETE',
    token
  });
}

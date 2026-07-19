import { request } from '@/services/http';
import type { JellyfinUser, JellyfinUserPolicy } from '@/types/jellyfin';

export type CreateUserPayload = {
  name: string;
  password?: string;
};

export type UpdateUserPolicyPayload = Partial<JellyfinUserPolicy>;

export function users(token: string) {
  return request<JellyfinUser[]>('/Users', { token });
}

export function createUser(token: string, payload: CreateUserPayload) {
  return request<JellyfinUser>('/Users/New', {
    method: 'POST',
    token,
    body: {
      Name: payload.name,
      Password: payload.password || undefined
    }
  });
}

export function updateUserPassword(token: string, userId: string, password: string, currentPassword?: string) {
  return request<void>(`/Users/${encodeURIComponent(userId)}/Password`, {
    method: 'POST',
    token,
    body: {
      NewPw: password,
      CurrentPw: currentPassword || undefined
    }
  });
}

export function updateUserPolicy(token: string, userId: string, payload: UpdateUserPolicyPayload) {
  return request<void>(`/Users/${encodeURIComponent(userId)}/Policy`, {
    method: 'POST',
    token,
    body: payload
  });
}

export function deleteUser(token: string, userId: string) {
  return request<void>(`/Users/${encodeURIComponent(userId)}`, {
    method: 'DELETE',
    token
  });
}

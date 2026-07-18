import { deviceId, request } from '@/services/http';
import type { AuthenticationResult, JellyfinUser } from '@/types/jellyfin';

export type LoginPayload = {
  username: string;
  password: string;
};

export function login(payload: LoginPayload) {
  return request<AuthenticationResult>('/Users/AuthenticateByName', {
    method: 'POST',
    body: {
      Username: payload.username,
      Pw: payload.password,
      DeviceId: deviceId()
    }
  });
}

export function currentUser(token: string) {
  return request<JellyfinUser>('/Users/Me', { token });
}

export function logout(token: string) {
  return request<void>('/Sessions/Logout', {
    method: 'POST',
    token
  });
}

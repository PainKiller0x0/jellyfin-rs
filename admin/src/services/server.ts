import { request } from '@/services/http';
import type {
  ActivityLogEntry,
  ItemCounts,
  PlaybackSession,
  QueryResult,
  ScheduledTask,
  SystemInfo
} from '@/types/server';

export function systemInfo(token: string) {
  return request<SystemInfo>('/System/Info', { token });
}

export function itemCounts(token: string) {
  return request<ItemCounts>('/Items/Counts', { token });
}

export function activeSessions(token: string) {
  return request<PlaybackSession[]>('/Sessions', { token });
}

export function scheduledTasks(token: string) {
  return request<ScheduledTask[]>('/ScheduledTasks', { token });
}

export function runScheduledTask(token: string, taskId: string) {
  return request<unknown>(`/ScheduledTasks/Running/${encodeURIComponent(taskId)}`, {
    method: 'POST',
    token
  });
}

export function stopScheduledTask(token: string, taskId: string) {
  return request<void>(`/ScheduledTasks/Running/${encodeURIComponent(taskId)}`, {
    method: 'DELETE',
    token
  });
}

export function activityLog(token: string, limit = 6) {
  return request<QueryResult<ActivityLogEntry>>(`/System/ActivityLog/Entries?Limit=${limit}`, {
    token
  });
}

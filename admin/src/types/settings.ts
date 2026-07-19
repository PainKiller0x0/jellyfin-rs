import type { QueryResult } from '@/types/server';

export type TmdbClientConfiguration = {
  IsTmdbEnabled: boolean;
  IsEnabled: boolean;
  Enabled: boolean;
  HasApiKey: boolean;
};

export type DoubanClientConfiguration = {
  IsDoubanEnabled: boolean;
  IsEnabled: boolean;
  Enabled: boolean;
  HasCookie: boolean;
};

export type ApiKey = {
  Id: string;
  AccessToken: string;
  AppName: string;
  AppVersion: string;
  DeviceName: string;
  UserId: string;
  IsActive: boolean;
  DateCreated: string;
  DateLastActivity: string;
  UserName?: string | null;
};

export type ApiKeyQueryResult = QueryResult<ApiKey>;

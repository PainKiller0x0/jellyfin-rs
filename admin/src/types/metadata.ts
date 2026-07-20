import type { QueryResult } from '@/types/server';

export type MetadataItemType = 'Movie' | 'Series' | 'Person';

export type ProviderIds = Record<string, string | number | null | undefined>;

export type AdminMediaItem = {
  Id: string;
  Name: string;
  Type: string;
  Path?: string | null;
  ParentId?: string | null;
  ProductionYear?: number | null;
  PremiereDate?: string | null;
  ProviderIds?: ProviderIds;
  ImageTags?: Record<string, string>;
  PrimaryImageTag?: string | null;
};

export type AdminMediaItemQueryResult = QueryResult<AdminMediaItem>;

export type RemoteSearchResult = {
  Name: string;
  Type?: MetadataItemType | string | null;
  ProductionYear?: number | null;
  PremiereDate?: string | null;
  SearchProviderName?: string | null;
  ProviderIds?: ProviderIds;
  ImageUrl?: string | null;
  Overview?: string | null;
  CommunityRating?: number | null;
  RuntimeTicks?: number | null;
  Genres?: string[];
  Tags?: string[];
  Studios?: unknown[];
  People?: unknown[];
  BackdropUrl?: string | null;
};

export type RemoteSearchPayload = {
  name: string;
  itemType: MetadataItemType;
  year?: number | null;
  providerIds?: ProviderIds;
};

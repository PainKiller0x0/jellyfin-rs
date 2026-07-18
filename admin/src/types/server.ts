export type QueryResult<T> = {
  Items: T[];
  TotalRecordCount: number;
  StartIndex: number;
};

export type SystemInfo = {
  ServerName: string;
  Version: string;
  Id: string;
  ServerId: string;
  StartupWizardCompleted: boolean;
  LocalAddress?: string;
  WanAddress?: string;
  OperatingSystem?: string;
  HasUpdateAvailable?: boolean;
};

export type ItemCounts = {
  MovieCount: number;
  SeriesCount: number;
  EpisodeCount: number;
  ArtistCount: number;
  ProgramCount: number;
  TrailerCount: number;
  SongCount: number;
  AlbumCount: number;
  MusicVideoCount: number;
  BoxSetCount: number;
  BookCount: number;
  ItemCount: number;
};

export type ActivityLogEntry = {
  Name: string;
  Type: string;
  Date: string;
  UserId: string;
  Severity: string;
};

export type ScheduledTaskResult = {
  Status?: string;
  StartTimeUtc?: string;
  EndTimeUtc?: string;
  ErrorMessage?: string | null;
};

export type ScheduledTask = {
  Name: string;
  State: string;
  Id: string;
  Key: string;
  Description: string;
  Category: string;
  IsHidden: boolean;
  LastExecutionResult?: ScheduledTaskResult | null;
};

export type PlaybackSession = {
  Id: string;
  UserId: string;
  Client: string;
  DeviceName: string;
  ApplicationVersion?: string;
  IsActive: boolean;
  LastActivityDate: string;
  NowPlayingItemName?: string | null;
};

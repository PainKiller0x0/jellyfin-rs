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

export type ScheduledTaskTrigger = {
  Type: string;
  TimeOfDayTicks?: number;
  IntervalTicks?: number;
  MaxRuntimeTicks?: number;
  DayOfWeek?: string;
  SystemEvent?: string;
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
  Triggers?: ScheduledTaskTrigger[];
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

export type AdminHttpLogEntry = {
  Id: number;
  Date: string;
  UnixTime: number;
  Method: string;
  Path: string;
  Query: string;
  StatusCode: number;
  ElapsedMs: number;
  RemoteAddress: string;
  Host: string;
  UserAgent: string;
  Client: string;
  Device: string;
  DeviceId: string;
};

export type AdminHttpLogResult = QueryResult<AdminHttpLogEntry> & {
  LastId: number;
};

export type PlaybackRegion = {
  Region: string;
  RegionCode: string;
  IsPrivate: boolean;
  PlayCount: number;
  UserCount: number;
  IpCount: number;
  SampleIps: string[];
  LastSeenDate: string;
  X: number;
  Y: number;
};

export type PlaybackRecentEvent = {
  Date: string;
  UnixTime: number;
  UserId: string;
  Ip: string;
  Region: string;
  Client: string;
  DeviceName: string;
  ItemId: string;
  ItemName?: string | null;
};

export type PlaybackMap = {
  TotalPlayCount: number;
  RegionCount: number;
  Regions: PlaybackRegion[];
  RecentEvents: PlaybackRecentEvent[];
};

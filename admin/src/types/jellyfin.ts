export type JellyfinUserPolicy = {
  IsAdministrator?: boolean;
  IsDisabled?: boolean;
  IsHidden?: boolean;
  EnableCollectionManagement?: boolean;
  EnableSubtitleManagement?: boolean;
  EnableLyricManagement?: boolean;
  EnableRemoteControlOfOtherUsers?: boolean;
  EnableSharedDeviceControl?: boolean;
  EnableLiveTvManagement?: boolean;
  EnableLiveTvAccess?: boolean;
  EnableAudioPlaybackTranscoding?: boolean;
  EnableVideoPlaybackTranscoding?: boolean;
  ForceRemoteSourceTranscoding?: boolean;
  EnableContentDeletion?: boolean;
  EnableSyncTranscoding?: boolean;
  EnableMediaConversion?: boolean;
  EnablePublicSharing?: boolean;
  EnableUserPreferenceAccess?: boolean;
  EnableRemoteAccess?: boolean;
  EnableMediaPlayback?: boolean;
  EnablePlaybackRemuxing?: boolean;
  EnableContentDownloading?: boolean;
  EnableAllDevices?: boolean;
  EnableAllChannels?: boolean;
  EnableAllFolders?: boolean;
  MaxActiveSessions?: number;
  RemoteClientBitrateLimit?: number;
  LoginAttemptsBeforeLockout?: number;
  SyncPlayAccess?: string;
  EnabledDevices?: string[];
  EnabledChannels?: string[];
  EnabledFolders?: string[];
  BlockedMediaFolders?: string[];
  BlockedChannels?: string[];
  AllowedTags?: string[];
  BlockedTags?: string[];
};

export type JellyfinUser = {
  Id: string;
  Name: string;
  ServerId?: string;
  HasPassword?: boolean;
  Policy?: JellyfinUserPolicy;
};

export type AuthenticationResult = {
  User: JellyfinUser;
  AccessToken: string;
  ServerId: string;
};

export type ApiErrorBody = {
  Error?: string;
  Message?: string;
};

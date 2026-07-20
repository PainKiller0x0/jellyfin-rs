<script setup lang="ts">
import { ElMessage, ElMessageBox, type FormInstance, type FormRules } from 'element-plus';
import { computed, onMounted, reactive, ref } from 'vue';

import * as usersApi from '@/services/users';
import { useAuthStore } from '@/stores/auth';
import type { JellyfinUser, JellyfinUserPolicy } from '@/types/jellyfin';

type CreateForm = {
  name: string;
  password: string;
};

type PasswordForm = {
  userId: string;
  userName: string;
  currentPassword: string;
  password: string;
};

type PolicyForm = Required<
  Pick<
    JellyfinUserPolicy,
    | 'IsAdministrator'
    | 'IsDisabled'
    | 'IsHidden'
    | 'EnableRemoteAccess'
    | 'EnableMediaPlayback'
    | 'EnableUserPreferenceAccess'
    | 'EnableContentDownloading'
    | 'EnablePublicSharing'
    | 'EnablePlaybackRemuxing'
    | 'EnableAudioPlaybackTranscoding'
    | 'EnableVideoPlaybackTranscoding'
    | 'EnableSyncTranscoding'
    | 'EnableMediaConversion'
    | 'EnableAllFolders'
    | 'EnableAllDevices'
    | 'EnableAllChannels'
    | 'EnableCollectionManagement'
    | 'EnableSubtitleManagement'
    | 'EnableLyricManagement'
    | 'EnableRemoteControlOfOtherUsers'
    | 'EnableSharedDeviceControl'
    | 'EnableLiveTvManagement'
    | 'EnableLiveTvAccess'
    | 'EnableContentDeletion'
    | 'ForceRemoteSourceTranscoding'
  >
> & {
  MaxActiveSessions: number;
  MaxConcurrentStreams: number;
  RemoteClientBitrateLimit: number;
  LoginAttemptsBeforeLockout: number;
  SyncPlayAccess: string;
};

type PolicySwitchKey = {
  [K in keyof PolicyForm]: PolicyForm[K] extends boolean ? K : never;
}[keyof PolicyForm];

type PolicySwitchItem = {
  key: PolicySwitchKey;
  label: string;
  description: string;
  selfDisabled?: boolean;
};

type PolicyGroup = {
  title: string;
  description: string;
  items: PolicySwitchItem[];
};

const authStore = useAuthStore();
const loading = ref(false);
const saving = ref(false);
const actionUserId = ref('');
const userRows = ref<JellyfinUser[]>([]);
const createDialogVisible = ref(false);
const passwordDialogVisible = ref(false);
const policyDrawerVisible = ref(false);
const policyUser = ref<JellyfinUser | null>(null);
const createFormRef = ref<FormInstance>();
const passwordFormRef = ref<FormInstance>();

const createForm = reactive<CreateForm>({
  name: '',
  password: ''
});

const passwordForm = reactive<PasswordForm>({
  userId: '',
  userName: '',
  currentPassword: '',
  password: ''
});

const policyForm = reactive<PolicyForm>({
  IsAdministrator: false,
  IsDisabled: false,
  IsHidden: false,
  EnableRemoteAccess: true,
  EnableMediaPlayback: true,
  EnableUserPreferenceAccess: true,
  EnableContentDownloading: true,
  EnablePublicSharing: false,
  EnablePlaybackRemuxing: true,
  EnableAudioPlaybackTranscoding: false,
  EnableVideoPlaybackTranscoding: false,
  EnableSyncTranscoding: false,
  EnableMediaConversion: false,
  EnableAllFolders: true,
  EnableAllDevices: true,
  EnableAllChannels: true,
  EnableCollectionManagement: false,
  EnableSubtitleManagement: false,
  EnableLyricManagement: false,
  EnableRemoteControlOfOtherUsers: false,
  EnableSharedDeviceControl: false,
  EnableLiveTvManagement: false,
  EnableLiveTvAccess: false,
  EnableContentDeletion: false,
  ForceRemoteSourceTranscoding: false,
  MaxActiveSessions: 0,
  MaxConcurrentStreams: 0,
  RemoteClientBitrateLimit: 0,
  LoginAttemptsBeforeLockout: -1,
  SyncPlayAccess: 'None'
});

const createRules: FormRules<CreateForm> = {
  name: [{ required: true, message: '请输入用户名', trigger: 'blur' }]
};

const passwordRules: FormRules<PasswordForm> = {
  password: [{ required: true, message: '请输入新密码', trigger: 'blur' }]
};

const adminCount = computed(() => userRows.value.filter(user => user.Policy?.IsAdministrator).length);
const disabledCount = computed(() => userRows.value.filter(user => user.Policy?.IsDisabled).length);
const normalCount = computed(() => Math.max(0, userRows.value.length - adminCount.value));
const passwordConfiguredCount = computed(() => userRows.value.filter(user => user.HasPassword).length);
const userStats = computed(() => [
  {
    label: '用户总数',
    value: userRows.value.length,
    hint: '个'
  },
  {
    label: '管理员',
    value: adminCount.value,
    hint: '个'
  },
  {
    label: '普通用户',
    value: normalCount.value,
    hint: '个'
  },
  {
    label: '已禁用',
    value: disabledCount.value,
    hint: '个'
  }
]);
const passwordTargetIsSelf = computed(() => passwordForm.userId === authStore.user?.Id);
const policyTargetIsSelf = computed(() => policyUser.value?.Id === authStore.user?.Id);

const policyGroups: PolicyGroup[] = [
  {
    title: '账号',
    description: '控制用户是否可登录，以及是否出现在普通用户列表中。',
    items: [
      {
        key: 'IsAdministrator',
        label: '管理员',
        description: '允许访问管理后台和需要管理员权限的接口。'
      },
      {
        key: 'IsDisabled',
        label: '禁用账号',
        description: '阻止登录，后端会撤销该用户现有 Token。',
        selfDisabled: true
      },
      {
        key: 'IsHidden',
        label: '隐藏用户',
        description: '从普通用户选择列表中隐藏。'
      }
    ]
  },
  {
    title: '访问',
    description: '控制用户能否远程访问、播放媒体、下载内容和修改个人偏好。',
    items: [
      {
        key: 'EnableRemoteAccess',
        label: '远程访问',
        description: '允许从远程客户端访问服务。'
      },
      {
        key: 'EnableMediaPlayback',
        label: '媒体播放',
        description: '允许播放媒体内容。'
      },
      {
        key: 'EnableUserPreferenceAccess',
        label: '个人偏好',
        description: '允许修改语言、字幕等个人设置。'
      },
      {
        key: 'EnableContentDownloading',
        label: '内容下载',
        description: '允许下载媒体文件。'
      },
      {
        key: 'EnablePublicSharing',
        label: '公开分享',
        description: '允许创建公开分享链接。'
      }
    ]
  },
  {
    title: '播放与转码',
    description: '控制 remux、音视频转码、同步转码和媒体转换。',
    items: [
      {
        key: 'EnablePlaybackRemuxing',
        label: '播放 Remux',
        description: '允许播放时重新封装容器。'
      },
      {
        key: 'EnableAudioPlaybackTranscoding',
        label: '音频转码',
        description: '允许播放时音频转码。'
      },
      {
        key: 'EnableVideoPlaybackTranscoding',
        label: '视频转码',
        description: '允许播放时视频转码。'
      },
      {
        key: 'ForceRemoteSourceTranscoding',
        label: '强制远程源转码',
        description: '远程媒体源播放时强制转码。'
      },
      {
        key: 'EnableSyncTranscoding',
        label: '同步转码',
        description: '允许离线同步时转码。'
      },
      {
        key: 'EnableMediaConversion',
        label: '媒体转换',
        description: '允许使用媒体转换能力。'
      }
    ]
  },
  {
    title: '范围',
    description: '后端支持设备、频道和媒体库范围控制；当前界面提供总开关。',
    items: [
      {
        key: 'EnableAllFolders',
        label: '所有媒体库',
        description: '允许访问全部媒体库；关闭后可由后端保存 EnabledFolders 列表。'
      },
      {
        key: 'EnableAllDevices',
        label: '所有设备',
        description: '允许所有设备访问。'
      },
      {
        key: 'EnableAllChannels',
        label: '所有频道',
        description: '允许全部频道访问。'
      },
      {
        key: 'EnableLiveTvAccess',
        label: '直播电视访问',
        description: '允许访问 Live TV 内容。'
      }
    ]
  },
  {
    title: '管理能力',
    description: '这些权限会让普通用户获得部分媒体和服务管理能力。',
    items: [
      {
        key: 'EnableCollectionManagement',
        label: '合集管理',
        description: '允许管理合集。'
      },
      {
        key: 'EnableSubtitleManagement',
        label: '字幕管理',
        description: '允许管理字幕。'
      },
      {
        key: 'EnableLyricManagement',
        label: '歌词管理',
        description: '允许管理歌词。'
      },
      {
        key: 'EnableRemoteControlOfOtherUsers',
        label: '控制其他用户',
        description: '允许远程控制其他用户的会话。'
      },
      {
        key: 'EnableSharedDeviceControl',
        label: '共享设备控制',
        description: '允许控制共享设备。'
      },
      {
        key: 'EnableLiveTvManagement',
        label: '直播电视管理',
        description: '允许管理 Live TV。'
      },
      {
        key: 'EnableContentDeletion',
        label: '删除内容',
        description: '允许删除媒体内容。'
      }
    ]
  }
];

async function loadUsers() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  try {
    userRows.value = await usersApi.users(authStore.token);
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载用户失败');
  } finally {
    loading.value = false;
  }
}

function resetCreateForm() {
  createForm.name = '';
  createForm.password = '';
  createFormRef.value?.clearValidate();
}

function openCreateDialog() {
  resetCreateForm();
  createDialogVisible.value = true;
}

function openPasswordDialog(user: JellyfinUser) {
  passwordForm.userId = user.Id;
  passwordForm.userName = user.Name;
  passwordForm.currentPassword = '';
  passwordForm.password = '';
  passwordFormRef.value?.clearValidate();
  passwordDialogVisible.value = true;
}

function openPolicyDrawer(user: JellyfinUser) {
  policyUser.value = user;
  applyPolicyToForm(user.Policy);
  policyDrawerVisible.value = true;
}

function applyPolicyToForm(policy?: JellyfinUserPolicy) {
  policyForm.IsAdministrator = Boolean(policy?.IsAdministrator);
  policyForm.IsDisabled = Boolean(policy?.IsDisabled);
  policyForm.IsHidden = Boolean(policy?.IsHidden);
  policyForm.EnableRemoteAccess = policy?.EnableRemoteAccess ?? true;
  policyForm.EnableMediaPlayback = policy?.EnableMediaPlayback ?? true;
  policyForm.EnableUserPreferenceAccess = policy?.EnableUserPreferenceAccess ?? true;
  policyForm.EnableContentDownloading = policy?.EnableContentDownloading ?? true;
  policyForm.EnablePublicSharing = Boolean(policy?.EnablePublicSharing);
  policyForm.EnablePlaybackRemuxing = policy?.EnablePlaybackRemuxing ?? true;
  policyForm.EnableAudioPlaybackTranscoding = Boolean(policy?.EnableAudioPlaybackTranscoding);
  policyForm.EnableVideoPlaybackTranscoding = Boolean(policy?.EnableVideoPlaybackTranscoding);
  policyForm.EnableSyncTranscoding = Boolean(policy?.EnableSyncTranscoding);
  policyForm.EnableMediaConversion = Boolean(policy?.EnableMediaConversion);
  policyForm.EnableAllFolders = policy?.EnableAllFolders ?? true;
  policyForm.EnableAllDevices = policy?.EnableAllDevices ?? true;
  policyForm.EnableAllChannels = policy?.EnableAllChannels ?? true;
  policyForm.EnableCollectionManagement = Boolean(policy?.EnableCollectionManagement);
  policyForm.EnableSubtitleManagement = Boolean(policy?.EnableSubtitleManagement);
  policyForm.EnableLyricManagement = Boolean(policy?.EnableLyricManagement);
  policyForm.EnableRemoteControlOfOtherUsers = Boolean(policy?.EnableRemoteControlOfOtherUsers);
  policyForm.EnableSharedDeviceControl = Boolean(policy?.EnableSharedDeviceControl);
  policyForm.EnableLiveTvManagement = Boolean(policy?.EnableLiveTvManagement);
  policyForm.EnableLiveTvAccess = Boolean(policy?.EnableLiveTvAccess);
  policyForm.EnableContentDeletion = Boolean(policy?.EnableContentDeletion);
  policyForm.ForceRemoteSourceTranscoding = Boolean(policy?.ForceRemoteSourceTranscoding);
  policyForm.MaxConcurrentStreams = concurrentPlaybackLimit(policy);
  policyForm.MaxActiveSessions = policyForm.MaxConcurrentStreams;
  policyForm.RemoteClientBitrateLimit = policy?.RemoteClientBitrateLimit ?? 0;
  policyForm.LoginAttemptsBeforeLockout = policy?.LoginAttemptsBeforeLockout ?? -1;
  policyForm.SyncPlayAccess = policy?.SyncPlayAccess ?? 'None';
}

function policyPayload(): usersApi.UpdateUserPolicyPayload {
  const maxConcurrentStreams = policyNumber(policyForm.MaxConcurrentStreams, 0, 0);

  return {
    IsAdministrator: policyForm.IsAdministrator,
    IsDisabled: policyForm.IsDisabled,
    IsHidden: policyForm.IsHidden,
    EnableRemoteAccess: policyForm.EnableRemoteAccess,
    EnableMediaPlayback: policyForm.EnableMediaPlayback,
    EnableUserPreferenceAccess: policyForm.EnableUserPreferenceAccess,
    EnableContentDownloading: policyForm.EnableContentDownloading,
    EnablePublicSharing: policyForm.EnablePublicSharing,
    EnablePlaybackRemuxing: policyForm.EnablePlaybackRemuxing,
    EnableAudioPlaybackTranscoding: policyForm.EnableAudioPlaybackTranscoding,
    EnableVideoPlaybackTranscoding: policyForm.EnableVideoPlaybackTranscoding,
    ForceRemoteSourceTranscoding: policyForm.ForceRemoteSourceTranscoding,
    EnableSyncTranscoding: policyForm.EnableSyncTranscoding,
    EnableMediaConversion: policyForm.EnableMediaConversion,
    EnableAllFolders: policyForm.EnableAllFolders,
    EnableAllDevices: policyForm.EnableAllDevices,
    EnableAllChannels: policyForm.EnableAllChannels,
    EnableCollectionManagement: policyForm.EnableCollectionManagement,
    EnableSubtitleManagement: policyForm.EnableSubtitleManagement,
    EnableLyricManagement: policyForm.EnableLyricManagement,
    EnableRemoteControlOfOtherUsers: policyForm.EnableRemoteControlOfOtherUsers,
    EnableSharedDeviceControl: policyForm.EnableSharedDeviceControl,
    EnableLiveTvManagement: policyForm.EnableLiveTvManagement,
    EnableLiveTvAccess: policyForm.EnableLiveTvAccess,
    EnableContentDeletion: policyForm.EnableContentDeletion,
    MaxActiveSessions: maxConcurrentStreams,
    MaxConcurrentStreams: maxConcurrentStreams,
    RemoteClientBitrateLimit: policyNumber(policyForm.RemoteClientBitrateLimit, 0, 0),
    LoginAttemptsBeforeLockout: policyNumber(policyForm.LoginAttemptsBeforeLockout, -1, -1),
    SyncPlayAccess: policyForm.SyncPlayAccess || 'None'
  };
}

function concurrentPlaybackLimit(policy?: JellyfinUserPolicy) {
  return policyNumber(policy?.MaxConcurrentStreams ?? policy?.MaxActiveSessions ?? 0, 0, 0);
}

function policyNumber(value: number, fallback: number, min: number) {
  const next = Number(value);
  return Number.isFinite(next) ? Math.max(min, next) : fallback;
}

async function submitCreate() {
  const formEl = createFormRef.value;
  if (!formEl || !authStore.token) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }

  saving.value = true;
  try {
    await usersApi.createUser(authStore.token, {
      name: createForm.name,
      password: createForm.password
    });
    ElMessage.success('用户已创建');
    createDialogVisible.value = false;
    resetCreateForm();
    await loadUsers();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '创建用户失败');
  } finally {
    saving.value = false;
  }
}

async function submitPassword() {
  const formEl = passwordFormRef.value;
  if (!formEl || !authStore.token) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }
  if (passwordTargetIsSelf.value && !passwordForm.currentPassword.trim()) {
    ElMessage.warning('请输入当前密码');
    return;
  }

  saving.value = true;
  try {
    await usersApi.updateUserPassword(
      authStore.token,
      passwordForm.userId,
      passwordForm.password,
      passwordForm.currentPassword
    );
    ElMessage.success('密码已更新');
    passwordDialogVisible.value = false;
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '更新密码失败');
  } finally {
    saving.value = false;
  }
}

async function toggleAdmin(user: JellyfinUser) {
  await updatePolicy(user, { IsAdministrator: !user.Policy?.IsAdministrator }, '权限已更新');
}

async function toggleDisabled(user: JellyfinUser) {
  await updatePolicy(user, { IsDisabled: !user.Policy?.IsDisabled }, user.Policy?.IsDisabled ? '用户已启用' : '用户已禁用');
}

async function submitPolicy() {
  if (!authStore.token || !policyUser.value) {
    return;
  }

  saving.value = true;
  actionUserId.value = policyUser.value.Id;
  try {
    await usersApi.updateUserPolicy(authStore.token, policyUser.value.Id, policyPayload());
    ElMessage.success('权限已保存');
    policyDrawerVisible.value = false;
    await loadUsers();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '保存权限失败');
  } finally {
    saving.value = false;
    actionUserId.value = '';
  }
}

async function updatePolicy(user: JellyfinUser, payload: usersApi.UpdateUserPolicyPayload, message: string) {
  if (!authStore.token) {
    return;
  }

  actionUserId.value = user.Id;
  try {
    await usersApi.updateUserPolicy(authStore.token, user.Id, payload);
    ElMessage.success(message);
    await loadUsers();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '更新用户失败');
  } finally {
    actionUserId.value = '';
  }
}

async function removeUser(user: JellyfinUser) {
  if (!authStore.token || user.Id === authStore.user?.Id) {
    return;
  }

  const confirmed = await ElMessageBox.confirm(`确认删除用户 "${user.Name}"？`, '删除用户', {
    type: 'warning',
    confirmButtonText: '删除',
    cancelButtonText: '取消'
  })
    .then(() => true)
    .catch(() => false);
  if (!confirmed) {
    return;
  }

  actionUserId.value = user.Id;
  try {
    await usersApi.deleteUser(authStore.token, user.Id);
    ElMessage.success('用户已删除');
    await loadUsers();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '删除用户失败');
  } finally {
    actionUserId.value = '';
  }
}

onMounted(loadUsers);
</script>

<template>
  <section class="admin-page users-page">
    <div class="users-page__heading">
      <div>
        <h1>用户</h1>
        <p>{{ userRows.length }} 个用户，{{ adminCount }} 个管理员，{{ passwordConfiguredCount }} 个已配置密码。</p>
      </div>
      <div class="users-page__heading-actions">
        <ElButton :loading="loading" @click="loadUsers">
          <ElIcon>
            <Refresh />
          </ElIcon>
          刷新
        </ElButton>
        <ElButton type="primary" @click="openCreateDialog">
          <ElIcon>
            <Plus />
          </ElIcon>
          新建用户
        </ElButton>
      </div>
    </div>

    <div class="users-page__stats">
      <div v-for="stat in userStats" :key="stat.label" class="users-page__stat">
        <span>{{ stat.label }}</span>
        <strong>{{ stat.value }}</strong>
        <small>{{ stat.hint }}</small>
      </div>
    </div>

    <div v-loading="loading" class="users-page__user-list">
      <article
        v-for="user in userRows"
        :key="user.Id"
        class="users-page__user-card"
        :class="{ 'is-disabled': user.Policy?.IsDisabled }"
      >
        <div class="users-page__user-main">
          <ElAvatar :size="46" class="users-page__avatar">
            {{ user.Name.slice(0, 1).toUpperCase() }}
          </ElAvatar>

          <div class="users-page__user-copy">
            <div class="users-page__user-title">
              <div>
                <h2>{{ user.Name }}</h2>
                <span>{{ user.Id }}</span>
              </div>
              <div class="users-page__tags">
                <ElTag :type="user.Policy?.IsAdministrator ? 'success' : 'info'" effect="plain">
                  {{ user.Policy?.IsAdministrator ? '管理员' : '用户' }}
                </ElTag>
                <ElTag :type="user.Policy?.IsDisabled ? 'danger' : 'success'" effect="plain">
                  {{ user.Policy?.IsDisabled ? '已禁用' : '正常' }}
                </ElTag>
                <ElTag :type="user.HasPassword ? 'success' : 'warning'" effect="plain">
                  {{ user.HasPassword ? '已设密码' : '未设密码' }}
                </ElTag>
              </div>
            </div>

            <div class="users-page__quick-state">
              <span>
                <ElIcon>
                  <Connection />
                </ElIcon>
                {{ user.Policy?.EnableRemoteAccess ?? true ? '允许远程访问' : '禁止远程访问' }}
              </span>
              <span>
                <ElIcon>
                  <VideoPlay />
                </ElIcon>
                {{ user.Policy?.EnableMediaPlayback ?? true ? '允许媒体播放' : '禁止媒体播放' }}
              </span>
              <span>
                <ElIcon>
                  <Download />
                </ElIcon>
                {{ user.Policy?.EnableContentDownloading ?? true ? '允许下载' : '禁止下载' }}
              </span>
              <span>
                <ElIcon>
                  <VideoCamera />
                </ElIcon>
                {{ concurrentPlaybackLimit(user.Policy) > 0 ? `同时播放 ${concurrentPlaybackLimit(user.Policy)} 路` : '不限同时播放' }}
              </span>
            </div>
          </div>
        </div>

        <div class="users-page__card-actions">
          <ElButton @click="openPolicyDrawer(user)">
            <ElIcon>
              <Setting />
            </ElIcon>
            权限
          </ElButton>
          <ElButton @click="openPasswordDialog(user)">
            <ElIcon>
              <Key />
            </ElIcon>
            密码
          </ElButton>
          <ElButton :loading="actionUserId === user.Id" @click="toggleAdmin(user)">
            <ElIcon>
              <UserFilled />
            </ElIcon>
            {{ user.Policy?.IsAdministrator ? '取消管理员' : '设为管理员' }}
          </ElButton>
          <ElButton :loading="actionUserId === user.Id" type="warning" @click="toggleDisabled(user)">
            <ElIcon>
              <SwitchButton />
            </ElIcon>
            {{ user.Policy?.IsDisabled ? '启用' : '禁用' }}
          </ElButton>
          <ElButton
            :disabled="user.Id === authStore.user?.Id"
            :loading="actionUserId === user.Id"
            type="danger"
            @click="removeUser(user)"
          >
            <ElIcon>
              <Delete />
            </ElIcon>
            删除
          </ElButton>
        </div>
      </article>

      <ElEmpty v-if="!loading && !userRows.length" :image-size="96" description="暂无用户">
        <ElButton type="primary" @click="openCreateDialog">
          <ElIcon>
            <Plus />
          </ElIcon>
          新建用户
        </ElButton>
      </ElEmpty>
    </div>

    <ElDialog v-model="createDialogVisible" title="新建用户" width="460px" @closed="resetCreateForm">
      <ElForm ref="createFormRef" :model="createForm" :rules="createRules" label-position="top">
        <ElFormItem label="用户名" prop="name">
          <ElInput v-model.trim="createForm.name" />
        </ElFormItem>
        <ElFormItem label="密码">
          <ElInput v-model="createForm.password" show-password type="password" />
        </ElFormItem>
      </ElForm>
      <template #footer>
        <ElButton @click="createDialogVisible = false">取消</ElButton>
        <ElButton :loading="saving" type="primary" @click="submitCreate">保存</ElButton>
      </template>
    </ElDialog>

    <ElDialog v-model="passwordDialogVisible" title="重置密码" width="460px">
      <ElForm ref="passwordFormRef" :model="passwordForm" :rules="passwordRules" label-position="top">
        <ElFormItem label="用户">
          <ElInput v-model="passwordForm.userName" disabled />
        </ElFormItem>
        <ElFormItem v-if="passwordTargetIsSelf" label="当前密码">
          <ElInput v-model="passwordForm.currentPassword" show-password type="password" />
        </ElFormItem>
        <ElFormItem label="新密码" prop="password">
          <ElInput v-model="passwordForm.password" show-password type="password" />
        </ElFormItem>
      </ElForm>
      <template #footer>
        <ElButton @click="passwordDialogVisible = false">取消</ElButton>
        <ElButton :loading="saving" type="primary" @click="submitPassword">保存</ElButton>
      </template>
    </ElDialog>

    <ElDrawer v-model="policyDrawerVisible" size="560px" class="users-page__policy-drawer" destroy-on-close>
      <template #header>
        <div class="users-page__policy-header">
          <ElAvatar :size="38">{{ policyUser?.Name.slice(0, 1).toUpperCase() }}</ElAvatar>
          <div>
            <h2>{{ policyUser?.Name ?? '用户权限' }}</h2>
            <span>{{ policyUser?.Id }}</span>
          </div>
        </div>
      </template>

      <div class="users-page__policy-body">
        <ElAlert
          v-if="policyTargetIsSelf"
          :closable="false"
          show-icon
          title="正在编辑当前登录用户，禁用账号已锁定以避免把自己踢下线。"
          type="warning"
        />

        <section v-for="group in policyGroups" :key="group.title" class="users-page__policy-group">
          <div class="users-page__policy-group-heading">
            <h3>{{ group.title }}</h3>
            <p>{{ group.description }}</p>
          </div>

          <div class="users-page__policy-switches">
            <label v-for="item in group.items" :key="item.key" class="users-page__policy-switch">
              <span>
                <strong>{{ item.label }}</strong>
                <small>{{ item.description }}</small>
              </span>
              <ElSwitch
                v-model="policyForm[item.key]"
                :disabled="item.selfDisabled && policyTargetIsSelf"
                :loading="saving && actionUserId === policyUser?.Id"
              />
            </label>
          </div>
        </section>

        <section class="users-page__policy-group">
          <div class="users-page__policy-group-heading">
            <h3>限制</h3>
            <p>0 表示不限制同时播放数量。</p>
          </div>

          <div class="users-page__policy-limits">
            <label>
              <span>同时播放限制</span>
              <ElInputNumber v-model="policyForm.MaxConcurrentStreams" :min="0" :step="1" controls-position="right" />
            </label>
            <label>
              <span>远程码率限制</span>
              <ElInputNumber
                v-model="policyForm.RemoteClientBitrateLimit"
                :min="0"
                :step="1_000_000"
                controls-position="right"
              />
            </label>
            <label>
              <span>锁定前登录失败次数</span>
              <ElInputNumber
                v-model="policyForm.LoginAttemptsBeforeLockout"
                :min="-1"
                :step="1"
                controls-position="right"
              />
            </label>
            <label>
              <span>SyncPlay</span>
              <ElSelect v-model="policyForm.SyncPlayAccess">
                <ElOption label="不允许" value="None" />
                <ElOption label="创建和加入" value="CreateAndJoinGroups" />
                <ElOption label="仅加入" value="JoinGroups" />
              </ElSelect>
            </label>
          </div>
        </section>
      </div>

      <template #footer>
        <div class="users-page__policy-footer">
          <ElButton @click="policyDrawerVisible = false">取消</ElButton>
          <ElButton :loading="saving" type="primary" @click="submitPolicy">保存权限</ElButton>
        </div>
      </template>
    </ElDrawer>
  </section>
</template>

<style scoped lang="scss">
.users-page {
  display: grid;
  align-content: start;
  gap: 16px;
  padding: 24px 32px 32px;
}

.users-page__heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;

  h1 {
    margin: 0;
    font-size: 24px;
    line-height: 1.25;
  }

  p {
    margin: 6px 0 0;
    color: var(--admin-muted);
    font-size: 14px;
  }
}

.users-page__heading-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  justify-content: flex-end;
}

.users-page__stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(120px, 1fr));
  gap: 12px;
}

.users-page__stat {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto auto;
  gap: 6px;
  align-items: baseline;
  min-height: 58px;
  padding: 12px 14px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;

  span {
    min-width: 0;
    overflow: hidden;
    color: var(--admin-muted);
    font-size: 13px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  strong {
    color: #0f766e;
    font-size: 24px;
    line-height: 1;
  }

  small {
    color: var(--admin-muted);
    font-size: 12px;
  }
}

.users-page__user-list {
  display: grid;
  align-content: start;
  gap: 12px;
  min-height: 260px;
}

.users-page__user-list :deep(.el-loading-mask) {
  border-radius: 8px;
}

.users-page__user-card {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 18px;
  align-items: start;
  padding: 16px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;
  box-shadow: 0 10px 26px rgba(15, 23, 42, 0.04);

  &.is-disabled {
    background: #fffafa;
  }
}

.users-page__user-main {
  display: flex;
  align-items: center;
  gap: 14px;
  min-width: 0;
}

.users-page__avatar {
  flex: 0 0 auto;
  color: #0f766e;
  background: #e6f4f1;
  font-weight: 700;
}

.users-page__user-copy {
  display: grid;
  min-width: 0;
  gap: 10px;
  flex: 1;
}

.users-page__user-title {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  min-width: 0;

  div {
    display: grid;
    min-width: 0;
    gap: 4px;
  }

  h2 {
    margin: 0;
    overflow: hidden;
    color: #0f172a;
    font-size: 17px;
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  span {
    overflow: hidden;
    color: var(--admin-muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.users-page__tags {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
  justify-content: flex-end;
}

.users-page__quick-state {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;

  span {
    display: inline-flex;
    align-items: center;
    min-height: 30px;
    gap: 6px;
    padding: 0 10px;
    border: 1px solid #e2e8f0;
    border-radius: 8px;
    color: #475569;
    background: #f8fafc;
    font-size: 13px;
    line-height: 1;
  }

  .el-icon {
    color: #0f766e;
    font-size: 14px;
  }
}

.users-page__card-actions {
  display: grid;
  grid-template-columns: repeat(2, max-content);
  gap: 8px;
  justify-content: end;
}

:deep(.users-page__policy-drawer .el-drawer__header) {
  margin: 0;
  padding: 18px 20px;
  border-bottom: 1px solid var(--admin-border);
}

:deep(.users-page__policy-drawer .el-drawer__body) {
  padding: 0;
  background: #f8fafc;
}

:deep(.users-page__policy-drawer .el-drawer__footer) {
  padding: 14px 20px;
  border-top: 1px solid var(--admin-border);
}

.users-page__policy-header {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;

  h2 {
    margin: 0;
    overflow: hidden;
    color: #0f172a;
    font-size: 18px;
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  span {
    display: block;
    overflow: hidden;
    max-width: 390px;
    color: var(--admin-muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.users-page__policy-body {
  display: grid;
  gap: 12px;
  padding: 16px;
}

.users-page__policy-group {
  display: grid;
  gap: 12px;
  padding: 14px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;
}

.users-page__policy-group-heading {
  display: grid;
  gap: 4px;

  h3,
  p {
    margin: 0;
  }

  h3 {
    color: #0f172a;
    font-size: 15px;
    line-height: 1.25;
  }

  p {
    color: var(--admin-muted);
    font-size: 12px;
    line-height: 1.45;
  }
}

.users-page__policy-switches {
  display: grid;
  gap: 8px;
}

.users-page__policy-switch {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 12px;
  align-items: center;
  min-height: 54px;
  padding: 10px 12px;
  border: 1px solid #edf2f7;
  border-radius: 8px;
  background: #fbfdff;

  span {
    display: grid;
    min-width: 0;
    gap: 3px;
  }

  strong,
  small {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  strong {
    color: #1f2937;
    font-size: 13px;
    line-height: 1.25;
    white-space: nowrap;
  }

  small {
    color: var(--admin-muted);
    font-size: 12px;
    line-height: 1.35;
  }
}

.users-page__policy-limits {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;

  label {
    display: grid;
    min-width: 0;
    gap: 7px;
  }

  span {
    color: #334155;
    font-size: 13px;
    font-weight: 700;
  }

  :deep(.el-input-number),
  :deep(.el-select) {
    width: 100%;
  }
}

.users-page__policy-footer {
  display: flex;
  gap: 10px;
  justify-content: flex-end;
}

@media (max-width: 760px) {
  .users-page {
    padding: 18px;
  }

  .users-page__heading {
    flex-direction: column;
  }

  .users-page__heading-actions {
    justify-content: flex-start;
  }

  .users-page__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .users-page__user-card {
    grid-template-columns: 1fr;
  }

  .users-page__user-title {
    display: grid;
  }

  .users-page__tags,
  .users-page__card-actions {
    justify-content: flex-start;
  }

  .users-page__policy-limits {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 540px) {
  .users-page__stats,
  .users-page__card-actions {
    grid-template-columns: 1fr;
  }

  .users-page__user-main {
    align-items: flex-start;
  }

  .users-page__quick-state {
    display: grid;
  }
}
</style>

export type Finding = {
  value: string;
  label: string;
};

export type Step = {
  name: string;
  detail: string;
};

export type Paper = {
  id: string;
  title: string;
  authors: string;
  venue: string;
  year: number;
  tags: string[];
  folder_id?: string;
  gist: string;
  problem: string;
  steps: Step[];
  findings: Finding[];
  limits: string;
  file?: string;
  source?: string;
  added?: number;
  read?: boolean;
  starred?: boolean;
  note?: string;
  doi?: string;
  origin?: string;
};

export type ImportResult = {
  created: string[];
  updated: string[];
  failed: string[];
  library: Library | null;
};

export type Settings = {
  has_key: boolean;
  key_hint: string;
  from_env: boolean;
  model: string;
  data_dir: string;
};

export type Library = {
  papers: Paper[];
  folders?: Folder[];
};

export type Folder = {
  id: string;
  name: string;
  parent_id: string;
  rev: number;
  deleted: boolean;
};

export type Placement = { paper_id: string; path: string[]; previous_folder_id: string };

export type SyncSettings = {
  enabled: boolean;
  auto: boolean;
  url: string;
  username: string;
  has_password: boolean;
  password_hint: string;
  insecure: boolean;
  last_sync: number;
};

export type SyncReport = {
  at: number;
  added: number;
  updated: number;
  deleted: number;
  conflicts: number;
  uploaded: boolean;
  pdf_up: number;
  pdf_down: number;
  message: string;
  library: Library | null;
};

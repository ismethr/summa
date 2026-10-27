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
};

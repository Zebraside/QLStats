/** QLStats — компоненты на чистом JS. Каждая функция возвращает DOM-узел. Типы — документация. */

export type LiveState = 'in_progress' | 'warmup' | 'empty' | 'offline';
export type Team = 'free' | 'red' | 'blue' | 'spectator';

export interface LivePlayer { steam_id: string; name: string; bot: boolean; team: Team; score: number | null; kills?: number | null }
export interface LiveResponse {
  generated_at: string; state: LiveState; server_title: string; offline_since?: string;
  match: null | {
    guid?: string; map: string; game_type: string; factory_title: string; team_mode: boolean; started_at: string;
    limits: { frag?: number; time_min?: number; capture?: number; round?: number; score?: number };
  };
  players: LivePlayer[]; spectators: number; score_approximate: boolean;
}
export interface LeaderRef { steam_id: string; name: string }
export interface Top { value: number; players: LeaderRef[] }
export interface SummaryResponse {
  generated_at: string;
  day: { date: string; matches: number; top_wins: Top; top_kills: Top };
  all_time: { matches: number; top_wins: Top; top_kills: Top };
}
export interface PlayerRow {
  steam_id: string; name: string; matches: number; kills: number; deaths: number; best_streak: number;
  wins: number; /** доля 0..1 или null */ accuracy: number | null; suicides: number; env_deaths: number;
}

export interface LiveMatchProps { loading?: boolean; /** Date.now() при получении ответа */ receivedAt?: number }
export declare function LiveMatch(live: LiveResponse | null, opts?: LiveMatchProps): HTMLElement;

export interface StatCardProps {
  label: string; value?: number; /** формы для 1, 2, 5 */ unit?: [string, string, string];
  /** не передавать для карточки без списка лидеров */ leaders?: LeaderRef[];
  emptyText?: string; loading?: boolean; max?: number;
}
export declare function StatCard(props: StatCardProps): HTMLElement;

export interface LeaderListProps { /** сколько показать до «+N», по умолчанию 3 */ max?: number }
export declare function LeaderList(players: LeaderRef[], opts?: LeaderListProps): HTMLUListElement;

export type SortKey = 'name' | 'matches' | 'kills' | 'deaths' | 'best_streak' | 'wins' | 'accuracy' | 'suicides' | 'env_deaths';
export interface PlayersTableProps {
  sort?: { key: SortKey; dir: 'asc' | 'desc' }; storageKey?: string; loading?: boolean; rows?: number; label?: string;
}
export declare function PlayersTable(players: PlayerRow[], opts?: PlayersTableProps): HTMLElement;

export interface UpdateIndicatorProps { status: 'ok' | 'loading' | 'error'; updatedAt?: number; retryAt?: number }
export declare function UpdateIndicator(props: UpdateIndicatorProps): HTMLParagraphElement;

export interface EmptyStateProps { kind: 'day' | 'error'; retryAt?: number; onRetry?: () => void }
export declare function EmptyState(props: EmptyStateProps): HTMLElement;

declare global {
  interface Window {
    QLStats: {
      LiveMatch: typeof LiveMatch; StatCard: typeof StatCard; LeaderList: typeof LeaderList;
      PlayersTable: typeof PlayersTable; UpdateIndicator: typeof UpdateIndicator; EmptyState: typeof EmptyState;
      SectionHead(title: string, sub?: string | null, id?: string): HTMLElement;
      icon(name: string, cls?: string): HTMLElement;
      startTicker(root?: ParentNode): () => void; tick(root?: ParentNode, now?: number): void;
      fmt: { num(n: number | null): string; accuracy(a: number | null): string; plural(n: number, forms: [string, string, string]): string; clock(sec: number): string; ago(sec: number): string; limits(l: object): string; mskTime(iso: string): string };
      /** только для прототипа */ sample: { players: PlayerRow[]; live: Record<string, LiveResponse>; summary: Record<string, SummaryResponse> };
    };
  }
}

// 加一门语言：放一个 `<tag>.ts`（键由 `MessageKey` 约束），再登记到下面的两处。
import { en } from './en';
import { zhCN } from './zh-CN';

export type { MessageKey } from './en';
export { en, zhCN };

export const dictionaries = { en, 'zh-CN': zhCN } as const;

export type LocaleName = keyof typeof dictionaries;
export const localeNames: LocaleName[] = ['en', 'zh-CN'];

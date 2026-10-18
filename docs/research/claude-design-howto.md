# Как работать с Claude Design (выжимка исследования)

Claude Design — продукт Anthropic Labs: холст для прототипов, слайдов и лендингов с дизайн-системой и передачей результата в Claude Code. Доступен на Pro/Max/Team/Enterprise.

Источники: [анонс](https://www.anthropic.com/news/claude-design-anthropic-labs), [Get started](https://support.claude.com/en/articles/14604416-get-started-with-claude-design), [Design system setup](https://support.claude.com/en/articles/14604397-set-up-your-design-system-in-claude-design), [Academy: прототипы и UX](https://academy.claude.com/tutorials/using-claude-design-for-prototypes-and-ux), [блог дизайнера продукта](https://claude.com/blog/how-the-product-designer-who-built-claude-design-uses-it-to-explore-ideas-before-building-them).

## Входы
- Текстовый промпт, изображения, DOCX/PPTX/XLSX, скриншоты, кодовая база (GitHub-репозиторий или локальный код через `/design-sync` в Claude Code), web capture.
- Для репозиториев советуют привязывать конкретную папку, а не весь репозиторий. Поддержка загрузки .md явно не задокументирована — надёжнее вставить выжимку в промпт, а полные документы дать через привязанную папку `docs/`.

## Рекомендации по промпту (официальные)
- Указать цель, layout, контент и аудиторию.
- Сначала основной layout и контент, потом взаимодействия, edge cases и полировка.
- Назвать целевые устройства (desktop, mobile).
- Описывать сценарий по шагам и число экранов/состояний.
- Просить 2–3 варианта и комбинировать лучшее; начинать с wireframe.
- Задать внешний вид явно (шрифты, цвета, настроение, референсы), иначе результат «generic».
- Просить состояния: empty, loading, error, много данных.
- Просить проверку доступности, контраста, иерархии.
- Фидбек давать конкретно («зазор 8px»).
- Единые имена компонентов для чистой передачи в код; решения фиксировать в чате.

## Передача в код
- Export → Claude Code handoff: бандл с файлами дизайна, чатом и README для модели; сгенерированный промпт с URL бандла вставляется в локальный Claude Code.
- Файловый экспорт: zip, PDF, PPTX, standalone HTML. Figma и design tokens официально не заявлены.
- В Claude Code команда `/design` создаёт, правит и синхронизирует проекты.

## Ограничения
- Нет истории версий; inline-комментарии иногда не появляются (дублировать в чат).
- Большие репозитории тормозят браузер.
- Логотипы и изображения не генерирует.
- Прототип не production-код: перенос в Rust/axum делается руками (через Claude Code).

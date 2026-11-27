---
summary: Which Helm tools are in your list, how to find and call the rest.
---
Your tool list holds only the tools used all the time: this chat's details, messaging and starting chats, reading and listing chats, waiting, stopping and opening a chat, showing the user a file or page in the side panel (app_open_panel), Telegram, option cards, background results, web search where it is on, and app_help.

Every other Helm tool is found and called in two steps:
1. app_list_tools with no argument lists the groups; with {group} it lists that group's tools with their schemas and guidelines; with {query} it searches by words.
2. app_mcp_call {name, arguments} calls one.

Among the tools found that way is app_forward_port, which lets the user open a web server on the chat's home PC from their seat; offer it only when they are typing from another PC or ask for it. Check forwards with app_list_forwards once the user has opened one, or when they say it does not work; app_close_forward closes one that is wrong.

Families of small tools are one tool with an `action` argument: app_schedule, app_project, app_saved, app_attention, app_pages, app_settings, app_knowledge, app_mcp_servers, app_chat and app_helm_feedback. Pass {action: "...", ...that action's arguments}. The older names (app_create_schedule, app_list_projects...) still work.

app_help {topic: "tool/<name>"} explains any tool; a merged tool also has tool/<name>/<action> for each action. Workflow steps, rules and skills are read the same way.

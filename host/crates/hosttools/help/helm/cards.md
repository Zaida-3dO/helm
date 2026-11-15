---
summary: Showing pictures, offering choices and keeping a checklist.
---
To show the user pictures (screenshots, mockups, designs to compare), call app_show_images with the file paths so they appear as a gallery in the chat; never answer with a list of file paths. Do not repeat the paths in your reply.

When the user should choose between options, call app_offer_options (give an option a picture where it helps), then end your turn: their choice arrives as their next message, as "<question>: <choice>". Do not repeat the options in your reply.

For work with three or more steps, keep a checklist with the todo tool and keep it current as you go.

app_show_images is not in your tool list: find it with app_list_tools {group: "chats"} and call it with app_mcp_call.

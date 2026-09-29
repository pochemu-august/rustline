import asyncio
import json
import threading
import tkinter as tk
from tkinter import ttk, messagebox

try:
    import websockets
except ImportError:
    print("Пожалуйста, установите библиотеку websockets: pip install websockets")
    raise


class RustlineClient:
    """Асинхронный WebSocket клиент для подключения к rustline-daemon."""

    def __init__(self, url: str = "ws://127.0.0.1:7890"):
        self.url = url
        self.ws = None
        self.request_id = 0
        self.on_event = None  
        self.on_response = None  

    async def connect(self):
        self.ws = await websockets.connect(self.url)
        asyncio.create_task(self._listen())

    async def _listen(self):
        try:
            async for message in self.ws:
                msg = json.loads(message)
                # Обработка событий (например: входящий звонок, смена статуса)
                if msg.get("method") == "event":
                    params = msg["params"]
                    if self.on_event:
                        self.on_event(params["event"], params["data"])
                # Обработка ответов на наши команды
                elif "result" in msg:
                    if self.on_response:
                        self.on_response(msg["id"], msg["result"])
                elif "error" in msg:
                    if self.on_response:
                        self.on_response(msg["id"], msg["error"])
        except websockets.ConnectionClosed:
            pass

    async def send(self, method: str, params: dict = None) -> int:
        self.request_id += 1
        request = {
            "jsonrpc": "2.0",
            "id": self.request_id,
            "method": method,
            "params": params or {},
        }
        await self.ws.send(json.dumps(request))
        return self.request_id

    async def register(self, server: str, username: str, password: str, **kwargs):
        return await self.send("register", {
            "server": server,
            "username": username,
            "password": password,
            **kwargs,
        })

    async def dial(self, target: str):
        return await self.send("dial", {"target": target})

    async def answer(self, call_id: str):
        return await self.send("answer", {"call_id": call_id})

    async def hangup(self, call_id: str):
        return await self.send("hangup", {"call_id": call_id})


class MicroSipCloneApp:
    """GUI на Tkinter в стиле MicroSIP для rustline."""

    def __init__(self):
        self.root = tk.Tk()
        self.root.title("rustline — SIP Client")
        self.root.geometry("320x480")  # Компактный размер как у MicroSIP
        self.root.resizable(False, False)
        
        # Настройка стиля
        style = ttk.Style()
        style.theme_use('clam')  # Более современный вид
        
        self.client = RustlineClient()
        self.loop = None
        self.current_call_id = None

        self._build_ui()
        self._start_async()

    def _build_ui(self):
        # --- Вкладки ---
        self.notebook = ttk.Notebook(self.root)
        self.notebook.pack(fill="both", expand=True, padx=5, pady=5)

        self.tab_dialer = ttk.Frame(self.notebook)
        self.tab_account = ttk.Frame(self.notebook)
        self.tab_log = ttk.Frame(self.notebook)

        self.notebook.add(self.tab_dialer, text="Номеронабиратель")
        self.notebook.add(self.tab_account, text="Аккаунт")
        self.notebook.add(self.tab_log, text="Журнал")

        self._build_dialer_tab()
        self._build_account_tab()
        self._build_log_tab()

        # --- Статус бар (внизу) ---
        self.status_var = tk.StringVar(value="Отключено")
        self.lbl_status = ttk.Label(self.root, textvariable=self.status_var, relief="sunken", anchor="w")
        self.lbl_status.pack(side="bottom", fill="x")

    def _build_dialer_tab(self):
        # Поле для ввода номера
        self.ent_target = ttk.Entry(self.tab_dialer, font=("Arial", 18), justify="center")
        self.ent_target.pack(fill="x", padx=15, pady=15)
        self.ent_target.insert(0, "101")

        # Сетка кнопок (Numpad)
        grid_frame = ttk.Frame(self.tab_dialer)
        grid_frame.pack(pady=10)

        buttons = [
            ('1', ''),   ('2', 'ABC'), ('3', 'DEF'),
            ('4', 'GHI'),('5', 'JKL'), ('6', 'MNO'),
            ('7', 'PQRS'),('8', 'TUV'), ('9', 'WXYZ'),
            ('*', ''),   ('0', '+'),   ('#', '')
        ]

        for i, (num, letters) in enumerate(buttons):
            row, col = divmod(i, 3)
            btn = ttk.Button(grid_frame, text=num, width=5, command=lambda n=num: self.ent_target.insert("end", n))
            btn.grid(row=row, column=col, padx=3, pady=3, ipady=5)

        # Кнопки Вызов / Сброс
        action_frame = ttk.Frame(self.tab_dialer)
        action_frame.pack(fill="x", padx=15, pady=10)
        
        self.btn_call = ttk.Button(action_frame, text="Вызов (Call)", command=self._on_call)
        self.btn_call.pack(side="left", expand=True, fill="x", padx=(0, 2))
        
        self.btn_hangup = ttk.Button(action_frame, text="Сброс (Hangup)", command=self._on_hangup, state="disabled")
        self.btn_hangup.pack(side="right", expand=True, fill="x", padx=(2, 0))

    def _build_account_tab(self):
        ttk.Label(self.tab_account, text="Сервер:").grid(row=0, column=0, sticky="w", padx=10, pady=10)
        self.ent_server = ttk.Entry(self.tab_account, width=25)
        self.ent_server.grid(row=0, column=1, padx=10)
        self.ent_server.insert(0, "192.168.0.104")

        ttk.Label(self.tab_account, text="Пользователь:").grid(row=1, column=0, sticky="w", padx=10, pady=10)
        self.ent_user = ttk.Entry(self.tab_account, width=25)
        self.ent_user.grid(row=1, column=1, padx=10)
        self.ent_user.insert(0, "100")

        ttk.Label(self.tab_account, text="Пароль:").grid(row=2, column=0, sticky="w", padx=10, pady=10)
        self.ent_pass = ttk.Entry(self.tab_account, width=25, show="*")
        self.ent_pass.grid(row=2, column=1, padx=10)
        self.ent_pass.insert(0, "100password")

        self.btn_register = ttk.Button(self.tab_account, text="Регистрация", command=self._on_register)
        self.btn_register.grid(row=3, column=0, columnspan=2, pady=20, ipadx=20)

    def _build_log_tab(self):
        self.txt_log = tk.Text(self.tab_log, state="disabled", font=("Consolas", 8), wrap="word")
        self.txt_log.pack(fill="both", expand=True, padx=5, pady=5)

    def _log(self, text: str):
        self.txt_log.config(state="normal")
        self.txt_log.insert("end", text + "\n")
        self.txt_log.see("end")
        self.txt_log.config(state="disabled")

    def _start_async(self):
        self.loop = asyncio.new_event_loop()
        thread = threading.Thread(target=self._run_loop, daemon=True)
        thread.start()
        self.loop.call_soon_threadsafe(asyncio.ensure_future, self._connect())

    def _run_loop(self):
        asyncio.set_event_loop(self.loop)
        self.loop.run_forever()

    async def _connect(self):
        try:
            self.client.on_event = self._on_event
            self.client.on_response = self._on_response
            await self.client.connect()
            self.root.after(0, lambda: self.status_var.set("Подключено к демону"))
            self.root.after(0, lambda: self._log("Успешное подключение к rustline-daemon"))
        except Exception as e:
            self.root.after(0, lambda: self._log(f"Ошибка подключения: {e}"))

    def _on_event(self, event_name, data):
        self.root.after(0, lambda: self._handle_event(event_name, data))

    def _on_response(self, req_id, result):
        self.root.after(0, lambda: self._log(f"Ответ [{req_id}]: {result}"))

    def _handle_event(self, event_name, data):
        self._log(f"Событие: {event_name}")
        if event_name == "registration_state_changed":
            state_ru = {"registered": "Зарегистрирован", "unregistered": "Не зарегистрирован", "registering": "Регистрация...", "failed": "Ошибка регистрации"}
            self.status_var.set(state_ru.get(data["state"], data["state"]))
            
        elif event_name == "incoming_call":
            name = data.get("caller_name", data["caller_uri"])
            if messagebox.askyesno("Входящий вызов", f"Звонок от {name}. Ответить?"):
                self.current_call_id = data["call_id"]
                asyncio.run_coroutine_threadsafe(self.client.answer(data["call_id"]), self.loop)
                self.btn_hangup.config(state="normal")
                
        elif event_name == "call_state_changed":
            if data["state"] == "disconnected":
                self.btn_hangup.config(state="disabled")
                self.current_call_id = None
                self.status_var.set("Вызов завершен")

    def _on_register(self):
        asyncio.run_coroutine_threadsafe(
            self.client.register(
                self.ent_server.get(),
                self.ent_user.get(),
                self.ent_pass.get(),
            ),
            self.loop,
        )

    def _on_call(self):
        target = self.ent_target.get().strip()
        if target:
            self.btn_hangup.config(state="normal")
            self.status_var.set(f"Звонок на {target}...")
            asyncio.run_coroutine_threadsafe(self.client.dial(target), self.loop)

    def _on_hangup(self):
        if self.current_call_id:
            asyncio.run_coroutine_threadsafe(self.client.hangup(self.current_call_id), self.loop)
        # Если вызов еще не установлен (состояние early/calling), но мы хотим сбросить
        self.ent_target.delete(0, 'end')

    def run(self):
        self.root.mainloop()

if __name__ == "__main__":
    MicroSipCloneApp().run()
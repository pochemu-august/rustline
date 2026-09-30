import asyncio
from datetime import datetime
import json
import os
import sys
import threading
import tkinter as tk
from tkinter import ttk, messagebox

try:
    import websockets
except ImportError:
    print("Пожалуйста, установите библиотеку websockets: pip install websockets")
    raise


class RustlineClient:
    """Асинхронный WebSocket клиент для rustline-daemon с авто-переподключением."""

    def __init__(self, url: str = "ws://127.0.0.1:7890"):
        self.url = url
        self.ws = None
        self.request_id = 0
        self.on_event = None
        self.on_response = None
        self.on_connection_change = None  # callback(connected: bool, message: str)
        self._closing = False
        self._connected = False

    @property
    def is_connected(self) -> bool:
        return self._connected and self.ws is not None

    async def connect_loop(self):
        """Непрерывный опрос и поддержание соединения с rustline-daemon."""
        attempt = 0
        while not self._closing:
            try:
                attempt += 1
                if not self._connected and self.on_connection_change:
                    self.on_connection_change(False, f"Поиск демона (попытка {attempt})...")

                self.ws = await websockets.connect(self.url)
                self._connected = True
                attempt = 0
                if self.on_connection_change:
                    self.on_connection_change(True, "Подключено к rustline-daemon")

                await self._listen()
            except Exception as e:
                self._connected = False
                self.ws = None
                if not self._closing and self.on_connection_change:
                    self.on_connection_change(False, f"Демон не запущен ({type(e).__name__}), повтор через 1.5с...")
            finally:
                self._connected = False
                self.ws = None

            if not self._closing:
                await asyncio.sleep(1.5)

    async def _listen(self):
        """Слушает входящие сообщения от сервера."""
        try:
            async for message in self.ws:
                msg = json.loads(message)
                if msg.get("method") == "event":
                    params = msg.get("params", {})
                    if self.on_event:
                        self.on_event(params.get("event"), params.get("data"))
                elif "result" in msg:
                    if self.on_response:
                        self.on_response(msg.get("id"), msg.get("result"))
                elif "error" in msg:
                    if self.on_response:
                        self.on_response(msg.get("id"), msg.get("error"))
        except websockets.ConnectionClosed:
            if not self._closing and self.on_connection_change:
                self.on_connection_change(False, "Соединение с демоном потеряно, переподключение...")

    async def send(self, method: str, params: dict = None) -> int:
        if not self.is_connected or self.ws is None:
            raise ConnectionError("rustline-daemon недоступен. Дождитесь подключения.")
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

    def close(self):
        self._closing = True


class MicroSipCloneApp:
    """GUI на Tkinter в стиле MicroSIP для rustline с логированием в общий файл."""

    def __init__(self):
        self.root = tk.Tk()
        self.root.title("rustline — SIP Client")
        self.root.geometry("340x500")
        self.root.resizable(False, False)

        # Файл логов (из переменной окружения или по умолчанию)
        self.log_file = os.environ.get("RUSTLINE_LOG_FILE", "rustline_session.log")

        style = ttk.Style()
        style.theme_use('clam')

        self.client = RustlineClient()
        self.loop = None
        self.current_call_id = None

        self._build_ui()
        self._start_async()

        self.root.protocol("WM_DELETE_WINDOW", self._on_close)

    def _build_ui(self):
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

        self.status_var = tk.StringVar(value="⏳ Поиск rustline-daemon...")
        self.lbl_status = ttk.Label(self.root, textvariable=self.status_var, relief="sunken", anchor="w", padding=(4, 2))
        self.lbl_status.pack(side="bottom", fill="x")

    def _build_dialer_tab(self):
        self.ent_target = ttk.Entry(self.tab_dialer, font=("Arial", 18), justify="center")
        self.ent_target.pack(fill="x", padx=15, pady=12)
        self.ent_target.insert(0, "101")

        grid_frame = ttk.Frame(self.tab_dialer)
        grid_frame.pack(pady=5)

        buttons = [
            ('1', ''),   ('2', 'ABC'), ('3', 'DEF'),
            ('4', 'GHI'),('5', 'JKL'), ('6', 'MNO'),
            ('7', 'PQRS'),('8', 'TUV'), ('9', 'WXYZ'),
            ('*', ''),   ('0', '+'),   ('#', '')
        ]

        for i, (num, letters) in enumerate(buttons):
            row, col = divmod(i, 3)
            btn = ttk.Button(grid_frame, text=num, width=5, command=lambda n=num: self.ent_target.insert("end", n))
            btn.grid(row=row, column=col, padx=3, pady=3, ipady=4)

        action_frame = ttk.Frame(self.tab_dialer)
        action_frame.pack(fill="x", padx=15, pady=10)

        self.btn_call = ttk.Button(action_frame, text="📞 Вызов (Call)", command=self._on_call)
        self.btn_call.pack(side="left", expand=True, fill="x", padx=(0, 2), ipady=3)

        self.btn_hangup = ttk.Button(action_frame, text="❌ Сброс (Hangup)", command=self._on_hangup, state="disabled")
        self.btn_hangup.pack(side="right", expand=True, fill="x", padx=(2, 0), ipady=3)

    def _build_account_tab(self):
        ttk.Label(self.tab_account, text="Сервер Asterisk:").grid(row=0, column=0, sticky="w", padx=10, pady=8)
        self.ent_server = ttk.Entry(self.tab_account, width=22)
        self.ent_server.grid(row=0, column=1, padx=10)
        self.ent_server.insert(0, "192.168.0.105")

        ttk.Label(self.tab_account, text="Пользователь (Ext):").grid(row=1, column=0, sticky="w", padx=10, pady=8)
        self.ent_user = ttk.Entry(self.tab_account, width=22)
        self.ent_user.grid(row=1, column=1, padx=10)
        self.ent_user.insert(0, "100")

        ttk.Label(self.tab_account, text="Пароль:").grid(row=2, column=0, sticky="w", padx=10, pady=8)
        self.ent_pass = ttk.Entry(self.tab_account, width=22, show="*")
        self.ent_pass.grid(row=2, column=1, padx=10)
        self.ent_pass.insert(0, "100password")

        self.btn_register = ttk.Button(self.tab_account, text="Регистрация на PBX", command=self._on_register)
        self.btn_register.grid(row=3, column=0, columnspan=2, pady=16, ipadx=15, ipady=4)

    def _build_log_tab(self):
        self.txt_log = tk.Text(self.tab_log, state="disabled", font=("Consolas", 8), wrap="word")
        self.txt_log.pack(fill="both", expand=True, padx=5, pady=5)

    def _log(self, text: str):
        now_str = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        log_line = f"[{now_str}] [GUI] {text}"
        
        # 1. Вывод в консоль
        print(log_line, flush=True)

        # 2. Запись в общий сессионный файл логов
        try:
            with open(self.log_file, "a", encoding="utf-8") as f:
                f.write(log_line + "\n")
        except Exception:
            pass

        # 3. Добавление во вкладку журнала GUI
        self.txt_log.config(state="normal")
        self.txt_log.insert("end", f"[{now_str[-8:]}] {text}\n")
        self.txt_log.see("end")
        self.txt_log.config(state="disabled")

    def _start_async(self):
        self.loop = asyncio.new_event_loop()
        thread = threading.Thread(target=self._run_loop, daemon=True)
        thread.start()

        self.client.on_event = self._on_event
        self.client.on_response = self._on_response
        self.client.on_connection_change = self._on_connection_change

        asyncio.run_coroutine_threadsafe(self.client.connect_loop(), self.loop)

    def _run_loop(self):
        asyncio.set_event_loop(self.loop)
        self.loop.run_forever()

    def _on_connection_change(self, connected: bool, message: str):
        self.root.after(0, lambda: self._handle_connection_change(connected, message))

    def _handle_connection_change(self, connected: bool, message: str):
        if connected:
            self.status_var.set("✅ Подключено к демону (готов)")
            self._log(f"{message}")
            self.btn_register.config(state="normal")
            self.btn_call.config(state="normal")
        else:
            self.status_var.set(f"⏳ {message}")
            self._log(f"{message}")
            self.btn_register.config(state="disabled")
            self.btn_call.config(state="disabled")

    def _on_event(self, event_name, data):
        self.root.after(0, lambda: self._handle_event(event_name, data))

    def _on_response(self, req_id, result):
        self.root.after(0, lambda: self._log(f"Ответ сервера [{req_id}]: {result}"))
        if isinstance(result, str) and ("@" in result or "-" in result):
            self.current_call_id = result

    def _handle_event(self, event_name, data):
        self._log(f"Событие от демона: {event_name} -> {data}")
        if event_name == "registration_state_changed":
            state = data.get("state")
            state_ru = {
                "registered": "✅ Зарегистрирован на PBX",
                "unregistered": "❌ Не зарегистрирован",
                "registering": "⏳ Регистрация...",
                "failed": "⚠️ Ошибка регистрации",
            }
            self.status_var.set(state_ru.get(state, state))

        elif event_name == "incoming_call":
            name = data.get("caller_name") or data.get("caller_uri") or "Unknown"
            call_id = data.get("call_id")
            if messagebox.askyesno("Входящий вызов", f"Входящий звонок от: {name}\nОтветить?"):
                self.current_call_id = call_id
                asyncio.run_coroutine_threadsafe(self.client.answer(call_id), self.loop)
                self.btn_hangup.config(state="normal")
                self.status_var.set(f"📞 Разговор с {name}")

        elif event_name == "call_state_changed":
            state = data.get("state")
            call_id = data.get("call_id")
            if state in ("early", "confirmed", "incoming"):
                self.current_call_id = call_id
                self.btn_hangup.config(state="normal")

            if state == "confirmed":
                self.status_var.set("🔊 Разговор (RTP звук активен)")
            elif state == "early":
                self.status_var.set("🔔 Гудки (180 Ringing)...")
            elif state == "disconnected":
                self.btn_hangup.config(state="disabled")
                self.current_call_id = None
                reason = data.get("reason")
                self.status_var.set(f"Вызов завершен ({reason})" if reason else "Вызов завершен")

    def _on_register(self):
        if not self.client.is_connected:
            self._log("⚠️ Ошибка: Демон еще не запущен или подключается!")
            messagebox.showwarning("Внимание", "rustline-daemon еще не готов. Дождитесь статуса подключения к демону.")
            return

        server = self.ent_server.get().strip()
        user = self.ent_user.get().strip()
        pwd = self.ent_pass.get().strip()
        self.status_var.set(f"⏳ Отправка REGISTER на {server}...")
        self._log(f"Отправка запроса регистрации {user}@{server}")
        asyncio.run_coroutine_threadsafe(
            self.client.register(server, user, pwd),
            self.loop,
        )

    def _on_call(self):
        if not self.client.is_connected:
            self._log("⚠️ Ошибка: Демон еще не запущен!")
            messagebox.showwarning("Внимание", "Дождитесь подключения к rustline-daemon.")
            return

        target = self.ent_target.get().strip()
        if target:
            self.btn_hangup.config(state="normal")
            self.status_var.set(f"📞 Вызов {target}...")
            self._log(f"Набор номера: {target}")
            asyncio.run_coroutine_threadsafe(self.client.dial(target), self.loop)

    def _on_hangup(self):
        if self.current_call_id:
            self._log(f"Завершение вызова {self.current_call_id}")
            asyncio.run_coroutine_threadsafe(self.client.hangup(self.current_call_id), self.loop)
        self.btn_hangup.config(state="disabled")
        self.status_var.set("Сброс вызова...")

    def _on_close(self):
        self.client.close()
        self.root.destroy()

    def run(self):
        self.root.mainloop()


if __name__ == "__main__":
    MicroSipCloneApp().run()
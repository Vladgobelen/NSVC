import sys
import ctypes
from PyQt5.QtWidgets import (QApplication, QMainWindow, QWidget, QVBoxLayout,
                             QHBoxLayout, QPushButton, QLabel, QLineEdit,
                             QGroupBox, QStatusBar)
from PyQt5.QtCore import QTimer

# Загружаем нашу Rust-библиотеку
try:
    lib = ctypes.CDLL("./libvoice_chat.so")  # Для Linux
except:
    try:
        lib = ctypes.CDLL("./voice_chat.dll")  # Для Windows
    except:
        lib = ctypes.CDLL("./libvoice_chat.dylib")  # Для macOS

# Определяем структуры и функции


class VoiceClient(ctypes.Structure):
    pass


# Определяем функции
voice_client_new = lib.voice_client_new
voice_client_new.argtypes = [ctypes.c_char_p, ctypes.c_uint16]
voice_client_new.restype = ctypes.c_void_p

voice_client_start = lib.voice_client_start
voice_client_start.argtypes = [ctypes.c_void_p]
voice_client_start.restype = ctypes.c_int

voice_client_set_transmitting = lib.voice_client_set_transmitting
voice_client_set_transmitting.argtypes = [ctypes.c_void_p, ctypes.c_bool]
voice_client_set_transmitting.restype = None

voice_client_stop = lib.voice_client_stop
voice_client_stop.argtypes = [ctypes.c_void_p]
voice_client_stop.restype = None

voice_client_free = lib.voice_client_free
voice_client_free.argtypes = [ctypes.c_void_p]
voice_client_free.restype = None

voice_client_set_bitrate = lib.voice_client_set_bitrate
voice_client_set_bitrate.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
voice_client_set_bitrate.restype = ctypes.c_int


class VoiceChatClient(QMainWindow):
    def __init__(self):
        super().__init__()
        self.client_ptr = None
        self.initUI()

    def initUI(self):
        self.setWindowTitle('Voice Chat Client')
        self.setGeometry(300, 300, 400, 300)

        # Центральный виджет
        central_widget = QWidget()
        self.setCentralWidget(central_widget)

        # Основной лейаут
        layout = QVBoxLayout(central_widget)

        # Группа подключения
        connection_group = QGroupBox("Подключение к серверу")
        connection_layout = QVBoxLayout()

        self.server_ip = QLineEdit("194.31.171.29")
        self.server_port = QLineEdit("38592")
        self.connect_btn = QPushButton("Подключиться")
        self.connect_btn.clicked.connect(self.connect_to_server)

        connection_layout.addWidget(QLabel("IP сервера:"))
        connection_layout.addWidget(self.server_ip)
        connection_layout.addWidget(QLabel("Порт сервера:"))
        connection_layout.addWidget(self.server_port)
        connection_layout.addWidget(self.connect_btn)

        connection_group.setLayout(connection_layout)
        layout.addWidget(connection_group)

        # Группа управления
        control_group = QGroupBox("Управление голосом")
        control_layout = QVBoxLayout()

        self.status_label = QLabel("Статус: Не подключено")
        self.talk_btn = QPushButton("Нажмите и говорите")
        self.talk_btn.setEnabled(False)
        self.talk_btn.pressed.connect(self.start_talking)
        self.talk_btn.released.connect(self.stop_talking)

        control_layout.addWidget(self.status_label)
        control_layout.addWidget(self.talk_btn)

        control_group.setLayout(control_layout)
        layout.addWidget(control_group)

        # Группа битрейта
        bitrate_group = QGroupBox("Настройки качества")
        bitrate_layout = QHBoxLayout()

        self.bitrate_label = QLabel("Битрейт: 64 кбит/с")
        self.bitrate_slider_btn = QPushButton("Установить 32 кбит/с")
        self.bitrate_slider_btn.setEnabled(False)
        self.bitrate_slider_btn.clicked.connect(self.toggle_bitrate)

        bitrate_layout.addWidget(self.bitrate_label)
        bitrate_layout.addWidget(self.bitrate_slider_btn)
        bitrate_group.setLayout(bitrate_layout)
        layout.addWidget(bitrate_group)

        # Статус бар
        self.statusBar().showMessage("Готово")

        # Таймер для обновления статуса
        self.status_timer = QTimer(self)
        self.status_timer.timeout.connect(self.update_status)
        self.status_timer.start(1000)  # Обновление каждую секунду

        # Переменные состояния
        self.is_connected = False
        self.is_talking = False
        self.current_bitrate = 64000

    def connect_to_server(self):
        if self.is_connected:
            self.disconnect_from_server()
            return

        ip = self.server_ip.text().encode('utf-8')
        try:
            port = int(self.server_port.text())
        except ValueError:
            self.statusBar().showMessage("Ошибка: Некорректный порт")
            return

        self.statusBar().showMessage("Соединяемся с сервером...")
        QApplication.processEvents()  # Обновляем UI

        self.client_ptr = voice_client_new(ip, port)
        if not self.client_ptr:
            self.statusBar().showMessage("Ошибка создания клиента!")
            return

        result = voice_client_start(self.client_ptr)
        if result != 0:
            self.statusBar().showMessage(f"Ошибка запуска клиента: {result}")
            voice_client_free(self.client_ptr)
            self.client_ptr = None
            return

        self.is_connected = True
        self.connect_btn.setText("Отключиться")
        self.talk_btn.setEnabled(True)
        self.bitrate_slider_btn.setEnabled(True)
        self.status_label.setText("Статус: Подключено, ожидание")
        self.statusBar().showMessage(f"Успешное подключение к {ip.decode()}:{port}")

    def disconnect_from_server(self):
        if not self.is_connected or not self.client_ptr:
            return

        if self.is_talking:
            self.stop_talking()

        voice_client_stop(self.client_ptr)
        voice_client_free(self.client_ptr)
        self.client_ptr = None

        self.is_connected = False
        self.connect_btn.setText("Подключиться")
        self.talk_btn.setEnabled(False)
        self.bitrate_slider_btn.setEnabled(False)
        self.status_label.setText("Статус: Не подключено")
        self.statusBar().showMessage("Отключено от сервера")

    def start_talking(self):
        if not self.is_connected or not self.client_ptr:
            return

        voice_client_set_transmitting(self.client_ptr, True)
        self.is_talking = True
        self.status_label.setText("Статус: Передача голоса")
        self.talk_btn.setText("Отпустите чтобы замолчать")
        self.statusBar().showMessage("Микрофон активирован")

    def stop_talking(self):
        if not self.is_connected or not self.client_ptr:
            return

        voice_client_set_transmitting(self.client_ptr, False)
        self.is_talking = False
        self.status_label.setText("Статус: Подключено, ожидание")
        self.talk_btn.setText("Нажмите и говорите")
        self.statusBar().showMessage("Микрофон деактивирован")

    def toggle_bitrate(self):
        if not self.is_connected or not self.client_ptr:
            return

        if self.current_bitrate == 64000:
            new_bitrate = 32000
            self.bitrate_label.setText("Битрейт: 32 кбит/с")
            self.bitrate_slider_btn.setText("Установить 64 кбит/с")
        else:
            new_bitrate = 64000
            self.bitrate_label.setText("Битрейт: 64 кбит/с")
            self.bitrate_slider_btn.setText("Установить 32 кбит/с")

        result = voice_client_set_bitrate(self.client_ptr, new_bitrate)
        if result == 0:
            self.current_bitrate = new_bitrate
            self.statusBar().showMessage(f"Битрейт изменен на {new_bitrate} бит/с")
        else:
            self.statusBar().showMessage(f"Ошибка изменения битрейта: {result}")

    def update_status(self):
        if not self.is_connected:
            return

        # Здесь можно добавить логику обновления статуса
        # Например, проверку активности сети

    def closeEvent(self, event):
        if self.is_connected:
            self.disconnect_from_server()
        event.accept()


if __name__ == '__main__':
    app = QApplication(sys.argv)
    window = VoiceChatClient()
    window.show()
    sys.exit(app.exec_())

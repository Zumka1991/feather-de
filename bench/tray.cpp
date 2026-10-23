// Build: c++ -fPIC bench/tray.cpp -o /tmp/feather-tray-fixture $(pkg-config --cflags --libs Qt6Widgets Qt6DBus)
// Run only on the private D-Bus session created by session-check.py.
#include <QApplication>
#include <QSystemTrayIcon>
#include <QMenu>
#include <QPainter>
#include <QTimer>
#include <QDBusInterface>
#include <QDBusConnection>
#include <cstdio>

int main(int argc,char **argv) {
    fprintf(stderr,"[feather-tray-fixture] starting\n");fflush(stderr);
    QApplication app(argc,argv);
    fprintf(stderr,"[feather-tray-fixture] application ready, tray=%d\n",QSystemTrayIcon::isSystemTrayAvailable());fflush(stderr);
    app.setApplicationName("FeatherTrayFixture");
    app.setQuitOnLastWindowClosed(false);
    QPixmap image(32,32); image.fill(Qt::transparent);
    QPainter p(&image);p.setRenderHint(QPainter::Antialiasing);
    p.setBrush(QColor("#1686f8"));p.setPen(Qt::NoPen);p.drawEllipse(2,2,28,28);
    p.setPen(QPen(Qt::white,3));p.drawLine(9,17,14,22);p.drawLine(14,22,24,11);p.end();
    auto populate=[&app](QMenu &menu){
    auto *run=menu.addAction("Run tray test");
    QObject::connect(run,&QAction::triggered,[]{fprintf(stderr,"[feather-tray-fixture] clicked\n");fflush(stderr);});
    auto *check=menu.addAction("Checked");check->setCheckable(true);check->setChecked(true);
    menu.addAction("Disabled")->setEnabled(false);
    auto *sub=menu.addMenu("Nested");sub->addAction("Child");
    menu.addSeparator();menu.addAction("Quit fixture",&app,&QCoreApplication::quit);
    };
    QMenu menu;populate(menu);
    QMenu secondMenu;populate(secondMenu);
    QSystemTrayIcon tray{QIcon(image)};tray.setToolTip("Feather tray test");tray.setContextMenu(&menu);tray.show();
    QSystemTrayIcon second(QIcon::fromTheme("audio-volume-high"));second.setToolTip("Second tray test");second.setContextMenu(&secondMenu);second.show();
    fprintf(stderr,"[feather-tray-fixture] icons shown\n");fflush(stderr);
    QTimer::singleShot(700,[]{
        QDBusInterface watcher("org.kde.StatusNotifierWatcher","/StatusNotifierWatcher","org.kde.StatusNotifierWatcher",QDBusConnection::sessionBus());
        fprintf(stderr,"[feather-tray-fixture] registered=%s\n",qPrintable(watcher.property("RegisteredStatusNotifierItems").toStringList().join(",")));fflush(stderr);
    });
    QTimer::singleShot(8000,[&]{second.hide();});
    QTimer::singleShot(9000,[&]{second.show();});
    QTimer::singleShot(30000,&app,&QCoreApplication::quit);
    return app.exec();
}

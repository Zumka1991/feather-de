#include <QApplication>
#include <QMainWindow>
#include <QMenuBar>
#include <QTimer>
#include <QTextEdit>
#include <QDebug>
#include <cstdio>
int main(int argc, char **argv) {
 QApplication app(argc,argv); app.setCursorFlashTime(0);
 app.setApplicationName("FeatherMenuTest");
 QMainWindow win;
 win.setWindowTitle("Feather menu test");
 auto *text=new QTextEdit("Menu actions appear here"); win.setCentralWidget(text);
 auto *file=win.menuBar()->addMenu("&File");
 auto *run=file->addAction("&Run test"); run->setShortcut(QKeySequence("Ctrl+R"));
 QObject::connect(run,&QAction::triggered,[text] { fprintf(stderr, "FEATHER_TEST_CLICKED\n"); text->setPlainText("Action executed"); });
 auto *disabled=file->addAction("Disabled item");disabled->setEnabled(false);
 file->addSeparator();
 auto *check=file->addAction("Checkable");check->setCheckable(true);check->setChecked(true);
 auto *nested=file->addMenu("Nested");auto *child=nested->addAction("Nested action");
 QObject::connect(child,&QAction::triggered,[] { fprintf(stderr, "FEATHER_TEST_NESTED_CLICKED\n"); });
 QObject::connect(file,&QMenu::aboutToShow,[] { fprintf(stderr, "FEATHER_TEST_ABOUT_TO_SHOW\n"); });
 auto *edit=win.menuBar()->addMenu("&Edit"); auto *change=edit->addAction("Change label");
 QObject::connect(change,&QAction::triggered,[run] { run->setText("Changed by action"); });
 QTimer::singleShot(3000,[run] { run->setText("Updated live"); fprintf(stderr, "FEATHER_TEST_LABEL_UPDATED\n"); });
 win.resize(640,420);win.show();QTimer::singleShot(15000,&app,&QCoreApplication::quit);return app.exec();
}
